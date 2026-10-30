//! Write-through: every workspace change lands in the config file. [`through`] runs a model
//! operation, derives the config edits it implies ([`change`]: [`plan`], a diff of the workspace
//! before and after) and applies them through config-core's `edit`, all under the [`Gate`]. The
//! file's reload then echoes the change back, and `Workspace::rebase` finds nothing to change.

use crate::config::DEFAULT_STORE;
use config_core::toml_edit::Value;
use config_core::{add_group, add_tab, move_tab, remove, set_field, At, EditError};
use flotilla_core::model::{Group, Tab, Workspace};
use std::path::Path;
use tauri::{AppHandle, Manager};

/// A `[[window.group.tab]]` as written.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Doc {
    pub title: String,
    pub url: String,
    pub detached: bool,
}

impl Doc {
    fn of(t: &Tab) -> Doc {
        Doc {
            title: t.title.clone(),
            url: t.home_url.clone(),
            detached: t.detached,
        }
    }
}

/// One change to the config. Windows and groups are addressed by title and name as the file has
/// them before the change; documents by their `url`. `url: None` addresses a group's head.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Write {
    NewGroup {
        window: String,
        name: String,
        url: String,
        store: String,
        collapsed: bool,
        detached: bool,
        docs: Vec<Doc>,
    },
    AddDoc {
        window: String,
        group: String,
        doc: Doc,
    },
    RemoveDoc {
        window: String,
        group: String,
        url: String,
    },
    RemoveGroup {
        window: String,
        group: String,
    },
    SetDetached {
        window: String,
        group: String,
        url: Option<String>,
        detached: bool,
    },
    SetCollapsed {
        window: String,
        group: String,
        collapsed: bool,
    },
    MoveDoc {
        window: String,
        group: String,
        url: String,
        to_window: String,
        to_group: String,
    },
    /// The document at `doc.url` leaves `group` to head a new group `name` (in the same store) at
    /// the end of `to_window`.
    MakeOwnGroup {
        window: String,
        group: String,
        doc: Doc,
        to_window: String,
        name: String,
        store: String,
    },
    SetTitle {
        window: String,
        group: String,
        url: String,
        title: String,
    },
    SetName {
        window: String,
        group: String,
        name: String,
    },
    SetUrl {
        window: String,
        group: String,
        url: Option<String>,
        new_url: String,
    },
}

/// `wanted`, or the first of "`wanted` 2", "`wanted` 3", … that no name in `existing` is
/// (compared trimmed, as the config compares group names).
pub fn unique_group_name(existing: &[&str], wanted: &str) -> String {
    let wanted = wanted.trim();
    let taken = |n: &str| existing.iter().any(|e| e.trim() == n);
    if !taken(wanted) {
        return wanted.to_string();
    }
    (2..)
        .map(|n| format!("{wanted} {n}"))
        .find(|n| !taken(n))
        .expect("an unbounded range finds a free name")
}

/// Runs `op` on `ws` and returns its result with the config writes it implies. A group the
/// operation named after one already in its window is renamed unique first, so the workspace and
/// the file agree.
pub fn change<R>(ws: &mut Workspace, op: impl FnOnce(&mut Workspace) -> R) -> (R, Vec<Write>) {
    let before = ws.clone();
    let r = op(ws);
    uniquify(&before, ws);
    let writes = plan(&before, ws);
    (r, writes)
}

/// Renames each group in `after` that is new or renamed since `before` and shares its name with
/// another group of its window.
fn uniquify(before: &Workspace, after: &mut Workspace) {
    for w in &mut after.windows {
        for gi in 0..w.groups.len() {
            let head = &w.groups[gi].head;
            let unchanged = before
                .windows
                .iter()
                .flat_map(|w| &w.groups)
                .any(|g| g.head.id == head.id && g.head.title == head.title);
            if unchanged {
                continue;
            }
            let others: Vec<&str> = w
                .groups
                .iter()
                .enumerate()
                .filter(|(i, _)| *i != gi)
                .map(|(_, g)| g.head.title.as_str())
                .collect();
            let name = unique_group_name(&others, &w.groups[gi].head.title);
            w.groups[gi].head.title = name;
        }
    }
}

/// Where a tab sits.
struct Loc<'a> {
    window: &'a str,
    group: &'a Group,
    tab: &'a Tab,
    head: bool,
}

fn locs(ws: &Workspace) -> Vec<Loc<'_>> {
    ws.windows
        .iter()
        .flat_map(|w| {
            w.groups.iter().flat_map(move |g| {
                std::iter::once((&g.head, true))
                    .chain(g.members.iter().map(|m| (m, false)))
                    .map(move |(tab, head)| Loc {
                        window: &w.title,
                        group: g,
                        tab,
                        head,
                    })
            })
        })
        .collect()
}

/// The config writes that take the file from `before` to `after`, in an order each can apply in
/// and every file state between them is a valid config: removals first (so nothing added is ever
/// listed twice), then new groups, moved and added documents, field changes, and renames last (so
/// every earlier write still finds a group by its old name).
pub fn plan(before: &Workspace, after: &Workspace) -> Vec<Write> {
    let (b, a) = (locs(before), locs(after));
    let in_b = |id: &str| b.iter().find(|l| l.tab.id == id);
    let in_a = |id: &str| a.iter().find(|l| l.tab.id == id);
    let mut groups = vec![];
    let mut docs = vec![];
    let mut fields = vec![];
    let mut removes = vec![];
    let mut renames = vec![];
    // A document whose id changed on a move across stores: matched by its URL.
    let moved_by_url = |m: &Loc| {
        b.iter()
            .find(|o| !o.head && o.tab.home_url == m.tab.home_url && in_a(&o.tab.id).is_none())
    };

    // A group is the same group only in the same window: one that changed window is written anew.
    let same_group = |o: &Loc, l: &Loc| o.head && o.window == l.window;
    for l in a.iter().filter(|l| l.head) {
        let g = l.group;
        match in_b(&g.head.id) {
            Some(o) if same_group(o, l) => {
                let (window, group) = (o.window.to_string(), o.group.head.title.clone());
                if o.group.collapsed != g.collapsed {
                    fields.push(Write::SetCollapsed {
                        window: window.clone(),
                        group: group.clone(),
                        collapsed: g.collapsed,
                    });
                }
                if o.tab.detached != g.head.detached {
                    fields.push(Write::SetDetached {
                        window: window.clone(),
                        group: group.clone(),
                        url: None,
                        detached: g.head.detached,
                    });
                }
                if o.tab.home_url != g.head.home_url {
                    fields.push(Write::SetUrl {
                        window: window.clone(),
                        group: group.clone(),
                        url: None,
                        new_url: g.head.home_url.clone(),
                    });
                }
                if o.tab.title != g.head.title {
                    renames.push(Write::SetName {
                        window,
                        group,
                        name: g.head.title.clone(),
                    });
                }
            }
            Some(o) if !o.head => groups.push(Write::MakeOwnGroup {
                window: o.window.to_string(),
                group: o.group.head.title.clone(),
                doc: Doc::of(o.tab),
                to_window: l.window.to_string(),
                name: g.head.title.clone(),
                store: g.store.clone(),
            }),
            _ => groups.push(Write::NewGroup {
                window: l.window.to_string(),
                name: g.head.title.clone(),
                url: g.head.home_url.clone(),
                store: g.store.clone(),
                collapsed: g.collapsed,
                detached: g.head.detached,
                docs: g.members.iter().map(Doc::of).collect(),
            }),
        }
    }

    for l in b.iter().filter(|l| l.head) {
        if !in_a(&l.tab.id).is_some_and(|n| same_group(l, n)) {
            removes.push(Write::RemoveGroup {
                window: l.window.to_string(),
                group: l.group.head.title.clone(),
            });
        }
    }

    for m in a.iter().filter(|l| !l.head) {
        // Members of a new group were written with it.
        let Some(home) = in_b(&m.group.head.id).filter(|o| same_group(o, m)) else {
            continue;
        };
        let (window, group) = (home.window.to_string(), home.group.head.title.clone());
        match in_b(&m.tab.id)
            .filter(|o| !o.head)
            .or_else(|| moved_by_url(m))
        {
            Some(o) if o.group.head.id == m.group.head.id => {
                let url = o.tab.home_url.clone();
                if o.tab.detached != m.tab.detached {
                    fields.push(Write::SetDetached {
                        window: window.clone(),
                        group: group.clone(),
                        url: Some(url.clone()),
                        detached: m.tab.detached,
                    });
                }
                if o.tab.title != m.tab.title {
                    fields.push(Write::SetTitle {
                        window: window.clone(),
                        group: group.clone(),
                        url: url.clone(),
                        title: m.tab.title.clone(),
                    });
                }
                if o.tab.home_url != m.tab.home_url {
                    fields.push(Write::SetUrl {
                        window,
                        group,
                        url: Some(url),
                        new_url: m.tab.home_url.clone(),
                    });
                }
            }
            Some(o) => docs.push(Write::MoveDoc {
                window: o.window.to_string(),
                group: o.group.head.title.clone(),
                url: o.tab.home_url.clone(),
                to_window: window,
                to_group: group,
            }),
            None => docs.push(Write::AddDoc {
                window,
                group,
                doc: Doc::of(m.tab),
            }),
        }
    }

    for o in b.iter().filter(|l| !l.head) {
        let gone = in_a(&o.tab.id).is_none()
            && !a
                .iter()
                .any(|m| !m.head && m.tab.home_url == o.tab.home_url && in_b(&m.tab.id).is_none());
        let group_stays = in_a(&o.group.head.id).is_some_and(|l| l.head && l.window == o.window);
        if gone && group_stays {
            removes.push(Write::RemoveDoc {
                window: o.window.to_string(),
                group: o.group.head.title.clone(),
                url: o.tab.home_url.clone(),
            });
        }
    }

    removes
        .into_iter()
        .chain(groups)
        .chain(docs)
        .chain(fields)
        .chain(renames)
        .collect()
}

/// The group's head (`url: None`) or the document at `url`.
fn tab_at<'a>(window: &'a str, group: &'a str, url: Option<&'a str>) -> At<'a> {
    At {
        window,
        group: Some(group),
        tab: url.map(|u| ("url", u)),
    }
}

/// `true` as a key's value; `false` removes the key (its default).
fn flag(on: bool) -> Option<Value> {
    on.then(|| Value::from(true))
}

fn doc_fields(d: &Doc) -> Vec<(&'static str, Value)> {
    let mut f = vec![
        ("title", Value::from(d.title.as_str())),
        ("url", Value::from(d.url.as_str())),
    ];
    f.extend(flag(d.detached).map(|v| ("detached", v)));
    f
}

fn group_fields(
    name: &str,
    url: &str,
    store: &str,
    collapsed: bool,
    detached: bool,
) -> Vec<(&'static str, Value)> {
    let mut f = vec![("name", Value::from(name)), ("url", Value::from(url))];
    if store != DEFAULT_STORE {
        f.push(("store", Value::from(store)));
    }
    f.extend(flag(collapsed).map(|v| ("collapsed", v)));
    f.extend(flag(detached).map(|v| ("detached", v)));
    f
}

/// One config-core edit: one atomic rewrite of the file.
pub(crate) enum Step {
    AddGroup {
        window: String,
        fields: Vec<(&'static str, Value)>,
    },
    AddTab {
        window: String,
        group: String,
        fields: Vec<(&'static str, Value)>,
    },
    /// The document at `url`, else the whole group.
    Remove {
        window: String,
        group: String,
        url: Option<String>,
    },
    /// `key` on the document at `url`, else on the group; `None` removes it.
    Set {
        window: String,
        group: String,
        url: Option<String>,
        key: &'static str,
        value: Option<Value>,
    },
    Move {
        window: String,
        group: String,
        url: String,
        to_window: String,
        to_group: String,
    },
}

impl Step {
    pub(crate) fn run(&self, path: &Path) -> Result<(), EditError> {
        match self {
            Step::AddGroup { window, fields } => add_group(path, window, fields),
            Step::AddTab {
                window,
                group,
                fields,
            } => add_tab(path, window, Some(group), fields),
            Step::Remove { window, group, url } => {
                remove(path, tab_at(window, group, url.as_deref()))
            }
            Step::Set {
                window,
                group,
                url,
                key,
                value,
            } => set_field(
                path,
                tab_at(window, group, url.as_deref()),
                key,
                value.clone(),
            ),
            Step::Move {
                window,
                group,
                url,
                to_window,
                to_group,
            } => move_tab(
                path,
                tab_at(window, group, Some(url)),
                to_window,
                Some(to_group),
            ),
        }
    }
}

/// The edits that make `w`, in an order where every file state between them is a valid config:
/// Make Own Group removes the document before adding the group it heads, so the document is
/// never listed twice.
pub(crate) fn steps(w: &Write) -> Vec<Step> {
    let set = |window: &str, group: &str, url: Option<&str>, key, value| Step::Set {
        window: window.into(),
        group: group.into(),
        url: url.map(Into::into),
        key,
        value,
    };
    match w.clone() {
        Write::NewGroup {
            window,
            name,
            url,
            store,
            collapsed,
            detached,
            docs,
        } => std::iter::once(Step::AddGroup {
            window: window.clone(),
            fields: group_fields(&name, &url, &store, collapsed, detached),
        })
        .chain(docs.iter().map(|d| Step::AddTab {
            window: window.clone(),
            group: name.clone(),
            fields: doc_fields(d),
        }))
        .collect(),
        Write::AddDoc { window, group, doc } => vec![Step::AddTab {
            window,
            group,
            fields: doc_fields(&doc),
        }],
        Write::RemoveDoc { window, group, url } => vec![Step::Remove {
            window,
            group,
            url: Some(url),
        }],
        Write::RemoveGroup { window, group } => vec![Step::Remove {
            window,
            group,
            url: None,
        }],
        Write::SetDetached {
            window,
            group,
            url,
            detached,
        } => vec![set(
            &window,
            &group,
            url.as_deref(),
            "detached",
            flag(detached),
        )],
        Write::SetCollapsed {
            window,
            group,
            collapsed,
        } => vec![set(&window, &group, None, "collapsed", flag(collapsed))],
        Write::MoveDoc {
            window,
            group,
            url,
            to_window,
            to_group,
        } => vec![Step::Move {
            window,
            group,
            url,
            to_window,
            to_group,
        }],
        Write::MakeOwnGroup {
            window,
            group,
            doc,
            to_window,
            name,
            store,
        } => vec![
            Step::Remove {
                window,
                group,
                url: Some(doc.url.clone()),
            },
            Step::AddGroup {
                window: to_window,
                fields: group_fields(&name, &doc.url, &store, false, doc.detached),
            },
        ],
        Write::SetTitle {
            window,
            group,
            url,
            title,
        } => vec![set(
            &window,
            &group,
            Some(&url),
            "title",
            Some(Value::from(title)),
        )],
        Write::SetName {
            window,
            group,
            name,
        } => vec![set(&window, &group, None, "name", Some(Value::from(name)))],
        Write::SetUrl {
            window,
            group,
            url,
            new_url,
        } => vec![set(
            &window,
            &group,
            url.as_deref(),
            "url",
            Some(Value::from(new_url)),
        )],
    }
}

/// Applies one write to the config at `path` through config-core's edits ([`steps`]).
pub fn apply(path: &Path, w: &Write) -> Result<(), EditError> {
    steps(w).iter().try_for_each(|s| s.run(path))
}

/// Serialises the app's config writes against its reloads. Held across a change and its writes
/// ([`through`]) and across each reload ([`Gate::reload`]), so a reload never reads a commit half
/// written, and a format-on-save rewrite never lands between two of a commit's edits. Taken
/// before `ws`.
///
/// Not reentrant: whatever runs under it must not reach anything that takes it again, which is
/// why it never covers view work (creating or closing a webview can run `intercept` synchronously,
/// and a main-thread task posted from the main thread runs inline). Taking it twice on one thread
/// panics rather than hangs.
#[derive(Default)]
pub struct Gate {
    lock: std::sync::Mutex<()>,
    owner: std::sync::Mutex<Option<std::thread::ThreadId>>,
}

/// The gate, held; released on drop.
pub struct Held<'a> {
    gate: &'a Gate,
    _lock: std::sync::MutexGuard<'a, ()>,
}

impl Drop for Held<'_> {
    fn drop(&mut self) {
        *self.gate.owner() = None;
    }
}

impl Gate {
    fn owner(&self) -> std::sync::MutexGuard<'_, Option<std::thread::ThreadId>> {
        self.owner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn hold(&self) -> Held<'_> {
        assert!(
            !self.held_here(),
            "the write gate was taken twice on one thread"
        );
        let lock = self.lock.lock().unwrap_or_else(|e| e.into_inner());
        *self.owner() = Some(std::thread::current().id());
        Held {
            gate: self,
            _lock: lock,
        }
    }

    /// Whether this thread holds the gate.
    pub fn held_here(&self) -> bool {
        *self.owner() == Some(std::thread::current().id())
    }

    /// Runs `apply` on the config file's contents, read and applied under the gate: whatever the
    /// watcher read before a commit finished is read again, whole, and nothing writes the file
    /// until `apply` (a format-on-save rewrite included) is done.
    pub fn reload<R>(&self, path: &Path, apply: impl FnOnce(std::io::Result<String>) -> R) -> R {
        let _held = self.hold();
        apply(std::fs::read_to_string(path))
    }
}

/// Runs `op` on the workspace and writes what it changed to the config, holding the gate across
/// both ([`through_with`]). A failed write reloads the workspace from the file; its views are
/// brought in line after the gate is released. The caller holds neither the gate nor `ws`, and
/// does its own view work afterwards as for any change.
pub fn through<R>(app: &AppHandle, op: impl FnOnce(&mut Workspace) -> R) -> R {
    let state = app.state::<crate::state::AppState>();
    through_with(
        &state.gate,
        &state.ws,
        &crate::config::config_path(),
        op,
        |e| {
            crate::tabs::notice(app, format!("Couldn't save to the config: {e}"));
            match crate::config::load(&crate::config::config_path()) {
                Ok(cfg) => crate::reload_model(app, cfg),
                Err(msg) => crate::tabs::notice(app, msg),
            }
        },
        || crate::refresh_views(app),
    )
}

/// [`through`]'s sequence, over its parts: under `gate`, `op` on `ws`, its writes to the config at
/// `path` (seeding the starter config first when there is none), and on a failed write
/// `reload(error)`, which must do no view work; then, released, `views()` if a write failed.
pub(crate) fn through_with<R>(
    gate: &Gate,
    ws: &std::sync::Mutex<Workspace>,
    path: &Path,
    op: impl FnOnce(&mut Workspace) -> R,
    reload: impl FnOnce(String),
    views: impl FnOnce(),
) -> R {
    let (r, failed) = {
        let _held = gate.hold();
        let (r, writes) = change(&mut ws.lock().unwrap(), op);
        let written = if writes.is_empty() {
            Ok(())
        } else {
            crate::config::create_starter(path).and_then(|_| {
                writes
                    .iter()
                    .try_for_each(|w| apply(path, w))
                    .map_err(|e| e.to_string())
            })
        };
        let failed = written.is_err();
        if let Err(e) = written {
            reload(e);
        }
        (r, failed)
    };
    if failed {
        views();
    }
    r
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_core::model::Opened;
    use url::Url;

    const F1: &str = "https://drive.google.com/drive/folders/f1";
    const F2: &str = "https://drive.google.com/drive/folders/f2";
    const F3: &str = "https://drive.google.com/drive/folders/f3";
    const D1: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit";
    const D2: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt02/edit";
    const D3: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt03/edit";

    /// Window A: "One" (F1; Budget D1, Minutes D2) and "Two" (F2). Window B: "Three" (F3, store
    /// work).
    fn source() -> String {
        format!(
            "# kept\n[[window]]\ntitle = \"A\"\n\
             [[window.group]]\nname = \"One\"\nurl = \"{F1}\" # the head\n\
             [[window.group.tab]]\ntitle = \"Budget\"\nurl = \"{D1}\"\n\
             [[window.group.tab]]\ntitle = \"Minutes\"\nurl = \"{D2}\"\n\
             [[window.group]]\nname = \"Two\"\nurl = \"{F2}\"\n\
             [[window]]\ntitle = \"B\"\n\
             [[window.group]]\nname = \"Three\"\nurl = \"{F3}\"\nstore = \"work\"\n"
        )
    }

    fn load(path: &Path) -> Workspace {
        let src = std::fs::read_to_string(path).unwrap();
        Workspace::from_config(&crate::config::parse(&src).unwrap().workspace_windows())
    }

    fn load_source() -> Workspace {
        Workspace::from_config(&crate::config::parse(&source()).unwrap().workspace_windows())
    }

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    fn id_of(ws: &Workspace, home: &str) -> String {
        ws.tabs().find(|t| t.home_url == home).unwrap().id.clone()
    }

    /// Runs `op` on the workspace the config holds, writes what it implies, and checks the
    /// reload is the workspace the operation left: the echo changes nothing. Returns the writes.
    fn run(op: impl FnOnce(&mut Workspace)) -> Vec<Write> {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, source()).unwrap();
        let mut ws = load(&path);
        let ((), writes) = change(&mut ws, op);
        for w in &writes {
            // Every file state between two edits is a valid config: open-once holds throughout.
            for s in steps(w) {
                s.run(&path).unwrap_or_else(|e| panic!("{w:?}: {e}"));
                let src = std::fs::read_to_string(&path).unwrap();
                crate::config::parse(&src).unwrap_or_else(|e| panic!("mid-{w:?}: {e}"));
            }
        }
        let mut echoed = ws.clone();
        echoed.rebase(load(&path));
        assert_eq!(
            echoed, ws,
            "the reload after {writes:?} changed the workspace"
        );
        let src = std::fs::read_to_string(&path).unwrap();
        assert!(src.starts_with("# kept"), "{src}");
        writes
    }

    fn one<T: Clone>(v: Vec<T>) -> T {
        assert_eq!(v.len(), 1);
        v[0].clone()
    }

    #[test]
    fn every_op_maps_to_one_write() {
        let w = one(run(|ws| {
            ws.new_group("B", &url(D3), "default").unwrap();
        }));
        assert!(matches!(&w, Write::NewGroup { window, name, .. } if window == "B" && name == D3));

        let w = one(run(|ws| {
            let h = id_of(ws, F2);
            assert!(matches!(ws.open_from(&h, &url(D3)), Some(Opened::Added(_))));
        }));
        assert!(
            matches!(&w, Write::AddDoc { group, doc, .. } if group == "Two" && doc.title == D3)
        );

        let w = one(run(|ws| ws.close_tab(&id_of(ws, D2))));
        assert_eq!(
            w,
            Write::RemoveDoc {
                window: "A".into(),
                group: "One".into(),
                url: D2.into()
            }
        );

        let w = one(run(|ws| ws.close_group(&id_of(ws, F1))));
        assert!(matches!(&w, Write::RemoveGroup { group, .. } if group == "One"));

        let w = one(run(|ws| ws.set_detached(&id_of(ws, D1), true)));
        assert!(matches!(&w, Write::SetDetached { url: Some(u), detached: true, .. } if u == D1));

        let w = one(run(|ws| ws.set_detached(&id_of(ws, F2), true)));
        assert!(matches!(&w, Write::SetDetached { url: None, group, .. } if group == "Two"));

        let w = one(run(|ws| ws.set_collapsed(&id_of(ws, F1), true)));
        assert!(matches!(
            &w,
            Write::SetCollapsed {
                collapsed: true,
                ..
            }
        ));

        let w = one(run(|ws| ws.move_to_group(&id_of(ws, D1), &id_of(ws, F2))));
        assert!(matches!(&w, Write::MoveDoc { to_group, .. } if to_group == "Two"));

        // Into another window, and another store: the id changes, the table moves.
        let w = one(run(|ws| ws.move_to_group(&id_of(ws, D1), &id_of(ws, F3))));
        assert!(
            matches!(&w, Write::MoveDoc { to_window, to_group, .. } if to_window == "B" && to_group == "Three")
        );

        let w = one(run(|ws| ws.make_own_group(&id_of(ws, D2))));
        assert!(matches!(&w, Write::MakeOwnGroup { name, .. } if name == "Minutes"));

        let w = one(run(|ws| ws.set_title(&id_of(ws, D1), "Budget 2026")));
        assert!(matches!(&w, Write::SetTitle { title, .. } if title == "Budget 2026"));

        let w = one(run(|ws| ws.set_title(&id_of(ws, F2), "Website")));
        assert!(matches!(&w, Write::SetName { name, .. } if name == "Website"));

        // Reopening a closed group writes it back whole, documents included; closed and reopened
        // in another window in one change, it leaves its old window too.
        let w = run(|ws| {
            ws.close_group(&id_of(ws, F1));
            ws.reopen_recent("B", 0).unwrap();
        });
        assert!(matches!(&w[0], Write::RemoveGroup { window, .. } if window == "A"));
        assert!(
            matches!(&w[1], Write::NewGroup { window, docs, .. } if window == "B" && docs.len() == 2)
        );
        assert_eq!(w.len(), 2);
        let mut closed = load_source();
        closed.close_group(&id_of(&closed, F1));
        let w = one(change(&mut closed, |ws| {
            ws.reopen_recent("A", 0).unwrap();
        })
        .1);
        assert!(matches!(&w, Write::NewGroup { window, .. } if window == "A"));

        // Activating a docked tab changes nothing in the file.
        assert!(run(|ws| ws.activate(&id_of(ws, F2))).is_empty());
    }

    #[test]
    fn a_name_taken_in_the_window_is_suffixed_in_the_model_and_the_file() {
        let w = one(run(|ws| ws.set_title(&id_of(ws, F2), "One")));
        assert!(matches!(&w, Write::SetName { name, .. } if name == "One 2"));
        // A document lifted into its own group under a name another group has.
        let w = one(run(|ws| {
            ws.set_title(&id_of(ws, D1), "Two");
            ws.make_own_group(&id_of(ws, D1));
        }));
        assert!(matches!(&w, Write::MakeOwnGroup { name, .. } if name == "Two 2"));
    }

    #[test]
    fn set_url_rewrites_the_head_or_the_document() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, source()).unwrap();
        let sub = "https://drive.google.com/drive/folders/sub";
        apply(
            &path,
            &Write::SetUrl {
                window: "A".into(),
                group: "One".into(),
                url: None,
                new_url: sub.into(),
            },
        )
        .unwrap();
        apply(
            &path,
            &Write::SetUrl {
                window: "A".into(),
                group: "One".into(),
                url: Some(D1.into()),
                new_url: D3.into(),
            },
        )
        .unwrap();
        let ws = load(&path);
        let g = &ws.windows[0].groups[0];
        assert_eq!(g.head.home_url, sub);
        assert_eq!(g.members[0].home_url, D3);
        assert_eq!(g.members.len(), 2);
    }

    /// A reload that arrives while a commit is between its edits waits for the gate, then reads
    /// the file whole.
    #[test]
    fn a_reload_mid_commit_sees_the_whole_commit() {
        use std::sync::mpsc;
        use std::time::Duration;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, source()).unwrap();
        let gate = std::sync::Arc::new(Gate::default());
        let (first_done, wait_first) = mpsc::channel();
        let (go_on, wait_go) = mpsc::channel::<()>();
        let writer = {
            let (gate, path) = (gate.clone(), path.clone());
            std::thread::spawn(move || {
                let _held = gate.hold();
                let w = Write::NewGroup {
                    window: "A".into(),
                    name: D3.into(),
                    url: D3.into(),
                    store: DEFAULT_STORE.into(),
                    collapsed: false,
                    detached: false,
                    docs: vec![],
                };
                apply(&path, &w).unwrap();
                first_done.send(()).unwrap();
                wait_go.recv().unwrap();
                apply(
                    &path,
                    &Write::SetName {
                        window: "A".into(),
                        group: D3.into(),
                        name: "Third".into(),
                    },
                )
                .unwrap();
            })
        };
        wait_first.recv().unwrap();
        let (seen, got) = mpsc::channel();
        let reader = {
            let (gate, path) = (gate.clone(), path.clone());
            std::thread::spawn(move || seen.send(gate.reload(&path, Result::unwrap)).unwrap())
        };
        // The commit is half done: the reload is held back.
        assert!(got.recv_timeout(Duration::from_millis(150)).is_err());
        go_on.send(()).unwrap();
        let src = got.recv_timeout(Duration::from_secs(5)).unwrap();
        writer.join().unwrap();
        reader.join().unwrap();
        assert!(src.contains("name = \"Third\""), "{src}");
    }

    #[test]
    #[should_panic(expected = "taken twice")]
    fn the_gate_refuses_to_be_taken_twice_on_one_thread() {
        let gate = Gate::default();
        let _first = gate.hold();
        let _second = gate.hold();
    }

    /// A commit whose edit fails reloads the model under the gate, and leaves every view hook
    /// (which may take the gate again, on the same thread) until the gate is released.
    #[test]
    fn a_failed_commit_does_its_view_work_after_the_gate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, source()).unwrap();
        let gate = Gate::default();
        let ws = std::sync::Mutex::new(load(&path));
        // A hand edit renamed group "Two" since the workspace was built: the write misses it.
        std::fs::write(&path, source().replace("\"Two\"", "\"Renamed\"")).unwrap();
        let (reloaded, viewed) = (std::cell::Cell::new(false), std::cell::Cell::new(false));
        through_with(
            &gate,
            &ws,
            &path,
            |ws| ws.set_collapsed(&id_of(ws, F2), true),
            |err| {
                assert!(err.contains("Two"), "{err}");
                assert!(gate.held_here(), "the model reloads under the gate");
                reloaded.set(true);
            },
            || {
                assert!(!gate.held_here(), "view work runs after the gate");
                let _again = gate.hold();
                viewed.set(true);
            },
        );
        assert!(reloaded.get() && viewed.get());
        // A commit that lands does no view work of its own: the caller's follows it.
        through_with(
            &gate,
            &ws,
            &path,
            |ws| ws.set_collapsed(&id_of(ws, F1), true),
            |e| panic!("{e}"),
            || panic!("no view work after a landed commit"),
        );
        assert!(!gate.held_here());
    }

    #[test]
    fn unique_group_name_suffixes() {
        assert_eq!(unique_group_name(&[], "Bulletins"), "Bulletins");
        assert_eq!(
            unique_group_name(&["Bulletins"], "Bulletins"),
            "Bulletins 2"
        );
        assert_eq!(
            unique_group_name(&[" Bulletins ", "Bulletins 2"], " Bulletins"),
            "Bulletins 3"
        );
        assert_eq!(unique_group_name(&["Minutes"], "Bulletins"), "Bulletins");
    }
}
