//! Write-through: every workspace change lands in the config file. [`change`] runs a model
//! operation and derives the config edits it implies ([`plan`], a diff of the workspace before and
//! after), [`commit`] applies them through config-core's `edit`. The file's reload then echoes the
//! change back, and `Workspace::rebase` finds nothing to change.

use crate::config::DEFAULT_STORE;
use config_core::toml_edit::Value;
use config_core::{add_group, add_tab, move_tab, remove, set_field, At, EditError};
use flotilla_core::model::{Group, Tab, Workspace};
use std::path::Path;
use tauri::AppHandle;

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

/// The config writes that take the file from `before` to `after`, in an order each can apply in:
/// new groups, then moved and added documents, then field changes, removals, and renames last (so
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

    groups
        .into_iter()
        .chain(docs)
        .chain(fields)
        .chain(removes)
        .chain(renames)
        .collect()
}

fn group_at<'a>(window: &'a str, group: &'a str) -> At<'a> {
    At {
        window,
        group: Some(group),
        tab: None,
    }
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

fn doc_fields(d: &Doc) -> Vec<(&str, Value)> {
    let mut f = vec![
        ("title", Value::from(d.title.as_str())),
        ("url", Value::from(d.url.as_str())),
    ];
    f.extend(flag(d.detached).map(|v| ("detached", v)));
    f
}

fn group_fields<'a>(
    name: &'a str,
    url: &'a str,
    store: &'a str,
    collapsed: bool,
    detached: bool,
) -> Vec<(&'a str, Value)> {
    let mut f = vec![("name", Value::from(name)), ("url", Value::from(url))];
    if store != DEFAULT_STORE {
        f.push(("store", Value::from(store)));
    }
    f.extend(flag(collapsed).map(|v| ("collapsed", v)));
    f.extend(flag(detached).map(|v| ("detached", v)));
    f
}

/// Applies one write to the config at `path` through config-core's edits.
pub fn apply(path: &Path, w: &Write) -> Result<(), EditError> {
    match w {
        Write::NewGroup {
            window,
            name,
            url,
            store,
            collapsed,
            detached,
            docs,
        } => {
            add_group(
                path,
                window,
                &group_fields(name, url, store, *collapsed, *detached),
            )?;
            for d in docs {
                add_tab(path, window, Some(name), &doc_fields(d))?;
            }
            Ok(())
        }
        Write::AddDoc { window, group, doc } => {
            add_tab(path, window, Some(group), &doc_fields(doc))
        }
        Write::RemoveDoc { window, group, url } => remove(path, tab_at(window, group, Some(url))),
        Write::RemoveGroup { window, group } => remove(path, group_at(window, group)),
        Write::SetDetached {
            window,
            group,
            url,
            detached,
        } => set_field(
            path,
            tab_at(window, group, url.as_deref()),
            "detached",
            flag(*detached),
        ),
        Write::SetCollapsed {
            window,
            group,
            collapsed,
        } => set_field(path, group_at(window, group), "collapsed", flag(*collapsed)),
        Write::MoveDoc {
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
        Write::MakeOwnGroup {
            window,
            group,
            doc,
            to_window,
            name,
            store,
        } => {
            add_group(
                path,
                to_window,
                &group_fields(name, &doc.url, store, false, doc.detached),
            )?;
            remove(path, tab_at(window, group, Some(&doc.url)))
        }
        Write::SetTitle {
            window,
            group,
            url,
            title,
        } => set_field(
            path,
            tab_at(window, group, Some(url)),
            "title",
            Some(Value::from(title.as_str())),
        ),
        Write::SetName {
            window,
            group,
            name,
        } => set_field(
            path,
            group_at(window, group),
            "name",
            Some(Value::from(name.as_str())),
        ),
        Write::SetUrl {
            window,
            group,
            url,
            new_url,
        } => set_field(
            path,
            tab_at(window, group, url.as_deref()),
            "url",
            Some(Value::from(new_url.as_str())),
        ),
    }
}

/// Applies `writes` to the config file, seeding the starter config first when there is none. A
/// write that fails stops the rest, shows why, and reloads the workspace from the file.
pub fn commit(app: &AppHandle, writes: &[Write]) {
    if writes.is_empty() {
        return;
    }
    let path = crate::config::config_path();
    let done = crate::config::create_starter(&path).and_then(|_| {
        writes
            .iter()
            .try_for_each(|w| apply(&path, w))
            .map_err(|e| e.to_string())
    });
    if let Err(e) = done {
        crate::tabs::notice(app, format!("Couldn't save to the config: {e}"));
        crate::reload_config(app);
    }
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
            apply(&path, w).unwrap_or_else(|e| panic!("{w:?}: {e}"));
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
        assert!(
            matches!(&w[0], Write::NewGroup { window, docs, .. } if window == "B" && docs.len() == 2)
        );
        assert!(matches!(&w[1], Write::RemoveGroup { window, .. } if window == "A"));
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
