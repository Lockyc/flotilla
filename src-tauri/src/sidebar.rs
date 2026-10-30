//! The chrome-core DTO for the workspace: what the sidebar renders.

use flotilla_core::identity::{kind, Kind};
use flotilla_core::model::{TabId, WindowWs, Workspace};
use serde_json::{json, Value};
use std::collections::HashSet;
use url::Url;

/// The Incoming row actions, as chrome-core's `onIncomingAction` reports them
/// ([`crate::commands::incoming_action`]).
pub const ADD_ACTIVE: &str = "add-active";
pub const ADD: &str = "add";
pub const NEW: &str = "new";
pub const DISMISS: &str = "dismiss";

const NO_GROUP: &str = "No group is open";

/// Window `title`'s whole chrome DTO: [`sidebar_dto`] plus its [`incoming_dto`].
pub fn window_dto(ws: &Workspace, title: &str, created: &HashSet<TabId>) -> Value {
    let mut dto = sidebar_dto(crate::tabs::window(ws, title), created);
    dto["incoming"] = incoming_dto(ws, title);
    dto
}

/// Window `window`'s Incoming queue as chrome-core's `incoming` rows: the link as the title, its
/// kind's glyph, the reason a rejected link can't be opened, and the actions. Add to the active
/// tab's group (named), Add to group… and New group are disabled with the reason on a rejected
/// link, and the adds while the window has no group to add to.
pub fn incoming_dto(ws: &Workspace, window: &str) -> Value {
    let Some(w) = crate::tabs::window(ws, window) else {
        return json!([]);
    };
    let active = ws.active_group(window).map(|g| g.head.title.as_str());
    let add_active = active.map_or_else(|| "Add to active group".into(), |t| format!("Add to {t}"));
    let no_group = |none: bool| none.then_some(NO_GROUP);
    let action = |id: &str, label: &str, disabled: Option<&str>| json!({ "id": id, "label": label, "disabled": disabled });
    let rows: Vec<Value> = w
        .incoming
        .iter()
        .map(|item| {
            let rejected = item.problem.as_deref();
            json!({
                "id": item.id,
                "title": item.link,
                "icon": icon_of(&item.link),
                "problem": item.problem,
                "actions": [
                    action(ADD_ACTIVE, &add_active, rejected.or(no_group(active.is_none()))),
                    action(ADD, "Add to group…", rejected.or(no_group(w.groups.is_empty()))),
                    action(NEW, "New group", rejected),
                    action(DISMISS, "Dismiss", None),
                ],
            })
        })
        .collect();
    Value::Array(rows)
}

/// The tab-list part of chrome-core's window DTO (`tabs`, `active`, `collapsed`); `chrome.js` adds
/// the window identity fields. Each group is a run of rows keyed by its head's id, the head row
/// first (`head: true`) and named by the head's title; removing a head removes its group, so its
/// bin asks for a second click (`confirmRemove`). `live` is whether the tab's webview exists
/// (`created`): the row's dot then offers Unload, and tab cycling skips cold rows. `icon` is the
/// glyph of the kind of Google page the tab opens on; other pages keep chrome-core's letter tile.
pub fn sidebar_dto(window: Option<&WindowWs>, created: &HashSet<TabId>) -> Value {
    let groups = window.map_or(&[][..], |w| &w.groups[..]);
    let tabs: Vec<Value> = groups
        .iter()
        .flat_map(|g| {
            std::iter::once((&g.head, true))
                .chain(g.members.iter().map(|m| (m, false)))
                .map(move |(t, head)| {
                    json!({
                        "id": t.id,
                        "title": t.title,
                        "group": g.head.id,
                        "groupName": g.head.title,
                        "head": head,
                        "confirmRemove": head,
                        "detached": t.detached,
                        "icon": icon_of(&t.home_url),
                        "live": created.contains(&t.id),
                        "attention": null,
                        "presence": null,
                        "killable": false,
                        "suspendable": false,
                        "startable": false,
                        "warn": false,
                    })
                })
        })
        .collect();
    let collapsed: Vec<&str> = groups
        .iter()
        .filter(|g| g.collapsed)
        .map(|g| g.head.id.as_str())
        .collect();
    let active = window.and_then(|w| w.active.as_ref());
    json!({ "active": active, "collapsed": collapsed, "tabs": tabs })
}

/// The glyph a tab opening on `home_url` shows, if it is a Google page.
fn icon_of(home_url: &str) -> Option<&'static str> {
    Url::parse(home_url).ok().as_ref().and_then(kind).map(glyph)
}

/// A kind's glyph, as a path into the chrome page's `src/`.
fn glyph(k: Kind) -> &'static str {
    match k {
        Kind::Doc => "icons/doc.svg",
        Kind::Sheet => "icons/sheet.svg",
        Kind::Slides => "icons/slides.svg",
        Kind::Form => "icons/form.svg",
        Kind::Folder => "icons/folder.svg",
        Kind::Drive => "icons/drive.svg",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tabs::testing::{new_group, one_window};
    use flotilla_core::model::Opened;
    use serde_json::json;
    use url::Url;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn sidebar_dto_orders_groups_and_marks_heads() {
        let mut ws = one_window();
        let Opened::Added(h1) = new_group(&mut ws, "https://drive.google.com/drive/folders/f1")
        else {
            panic!()
        };
        ws.set_title(&h1, "Auction 2026");
        let Some(Opened::Added(m1)) = ws.open_from(
            &h1,
            &url("https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit"),
        ) else {
            panic!()
        };
        ws.set_title(&m1, "Budget");
        let Opened::Added(h2) = new_group(&mut ws, "https://drive.google.com/drive/folders/f2")
        else {
            panic!()
        };
        ws.set_title(&h2, "Website");
        ws.set_collapsed(&h1, true);
        ws.set_detached(&m1, true);

        let dto = sidebar_dto(ws.windows.first(), &HashSet::from([h2.clone()]));
        assert_eq!(dto["active"], json!(h2));
        assert_eq!(dto["collapsed"], json!([h1]));
        let rows: Vec<(String, String, String, bool, bool)> = dto["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| {
                (
                    t["id"].as_str().unwrap().to_string(),
                    t["group"].as_str().unwrap().to_string(),
                    t["groupName"].as_str().unwrap().to_string(),
                    t["head"].as_bool().unwrap(),
                    t["detached"].as_bool().unwrap(),
                )
            })
            .collect();
        let row = |id: &str, g: &str, n: &str, head, det| {
            (id.to_string(), g.to_string(), n.to_string(), head, det)
        };
        assert_eq!(
            rows,
            [
                row(&h1, &h1, "Auction 2026", true, false),
                row(&m1, &h1, "Auction 2026", false, true),
                row(&h2, &h2, "Website", true, false),
            ]
        );
        assert_eq!(dto["tabs"][1]["title"], json!("Budget"));
        assert_eq!(dto["tabs"][0]["confirmRemove"], json!(true));
        assert_eq!(dto["tabs"][1]["confirmRemove"], json!(false));
        assert_eq!(dto["tabs"][0]["icon"], json!("icons/folder.svg"));
        assert_eq!(dto["tabs"][1]["icon"], json!("icons/doc.svg"));
        // Only h2 has a webview.
        let live: Vec<bool> = dto["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["live"].as_bool().unwrap())
            .collect();
        assert_eq!(live, [false, false, true]);
    }

    #[test]
    fn every_kind_has_a_glyph_and_other_pages_keep_the_letter_tile() {
        for (u, icon) in [
            (
                "https://docs.google.com/spreadsheets/d/x",
                "icons/sheet.svg",
            ),
            (
                "https://docs.google.com/presentation/d/x",
                "icons/slides.svg",
            ),
            ("https://docs.google.com/forms/d/x", "icons/form.svg"),
            ("https://drive.google.com/file/d/x", "icons/drive.svg"),
        ] {
            assert_eq!(icon_of(u), Some(icon), "{u}");
        }
        assert_eq!(icon_of("https://example.com/"), None);
        assert_eq!(icon_of("not a url"), None);
    }

    #[test]
    fn glyph_files_exist() {
        let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src");
        for k in [
            Kind::Doc,
            Kind::Sheet,
            Kind::Slides,
            Kind::Form,
            Kind::Folder,
            Kind::Drive,
        ] {
            assert!(src.join(glyph(k)).is_file(), "{}", glyph(k));
        }
    }

    const DOC: &str = "https://docs.google.com/document/d/1AbCdEfGhIjKlMnOpQrSt01/edit";

    /// Each row's actions as (id, label, disabled reason).
    fn actions(row: &Value) -> Vec<(String, String, Option<String>)> {
        row["actions"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| {
                (
                    a["id"].as_str().unwrap().to_string(),
                    a["label"].as_str().unwrap().to_string(),
                    a["disabled"].as_str().map(str::to_string),
                )
            })
            .collect()
    }

    fn act(id: &str, label: &str, disabled: Option<&str>) -> (String, String, Option<String>) {
        (id.into(), label.into(), disabled.map(str::to_string))
    }

    #[test]
    fn incoming_rows_carry_their_actions() {
        let mut ws = one_window();
        let Opened::Added(h) = new_group(&mut ws, "https://drive.google.com/drive/folders/f1")
        else {
            panic!()
        };
        ws.set_title(&h, "Fleet");
        ws.receive("W", None, DOC, "default").unwrap();
        ws.receive("W", None, "mailto:x", "default").unwrap();

        let dto = incoming_dto(&ws, "W");
        let rows = dto.as_array().unwrap();
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0]["title"], json!(DOC));
        assert_eq!(rows[0]["icon"], json!("icons/doc.svg"));
        assert_eq!(rows[0]["problem"], Value::Null);
        assert_eq!(
            actions(&rows[0]),
            [
                act(ADD_ACTIVE, "Add to Fleet", None),
                act(ADD, "Add to group…", None),
                act(NEW, "New group", None),
                act(DISMISS, "Dismiss", None),
            ]
        );
        let why = "Not a web link: mailto:x";
        assert_eq!(rows[1]["problem"], json!(why));
        assert_eq!(
            actions(&rows[1]),
            [
                act(ADD_ACTIVE, "Add to Fleet", Some(why)),
                act(ADD, "Add to group…", Some(why)),
                act(NEW, "New group", Some(why)),
                act(DISMISS, "Dismiss", None),
            ]
        );
        assert_eq!(incoming_dto(&ws, "nowhere"), json!([]));
    }

    #[test]
    fn incoming_with_no_group_disables_the_adds() {
        let mut ws = one_window();
        ws.receive("W", None, DOC, "default").unwrap();
        assert_eq!(
            actions(&incoming_dto(&ws, "W")[0]),
            [
                act(ADD_ACTIVE, "Add to active group", Some("No group is open")),
                act(ADD, "Add to group…", Some("No group is open")),
                act(NEW, "New group", None),
                act(DISMISS, "Dismiss", None),
            ]
        );
        // A group, but no tab active: only Add to group… is offered.
        let Opened::Added(_) = new_group(&mut ws, "https://drive.google.com/drive/folders/f1")
        else {
            panic!()
        };
        ws.windows[0].active = None;
        let a = actions(&incoming_dto(&ws, "W")[0]);
        assert_eq!(
            a[0],
            act(ADD_ACTIVE, "Add to active group", Some("No group is open"))
        );
        assert_eq!(a[1], act(ADD, "Add to group…", None));
    }

    #[test]
    fn the_window_dto_carries_its_own_queue() {
        let window = |t: &str| flotilla_core::model::ConfigWindow {
            title: t.into(),
            groups: vec![],
        };
        let mut ws = flotilla_core::model::Workspace::from_config(&[window("A"), window("B")]);
        ws.receive("B", None, DOC, "default").unwrap();
        let none = HashSet::new();
        assert_eq!(window_dto(&ws, "A", &none)["incoming"], json!([]));
        assert_eq!(
            window_dto(&ws, "B", &none)["incoming"],
            incoming_dto(&ws, "B")
        );
        assert_eq!(window_dto(&ws, "B", &none)["tabs"], json!([]));
    }

    #[test]
    fn empty_workspace_has_no_rows_and_no_active() {
        let dto = sidebar_dto(one_window().windows.first(), &HashSet::new());
        assert_eq!(dto, sidebar_dto(None, &HashSet::new()));
        assert_eq!(dto["active"], Value::Null);
        assert_eq!(dto["collapsed"], json!([]));
        assert_eq!(dto["tabs"], json!([]));
    }
}
