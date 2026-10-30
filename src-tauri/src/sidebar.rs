//! The chrome-core DTO for the workspace: what the sidebar renders.

use flotilla_core::model::{TabId, Workspace};
use serde_json::{json, Value};
use std::collections::HashSet;

/// The tab-list part of chrome-core's window DTO (`tabs`, `active`, `collapsed`); `chrome.js` adds
/// the window identity fields. Each group is a run of rows keyed by its head's id, the head row
/// first (`head: true`) and named by the head's title. `live` is whether the tab's webview exists
/// (`created`): the row's dot then offers Unload, and tab cycling skips cold rows.
pub fn sidebar_dto(ws: &Workspace, created: &HashSet<TabId>) -> Value {
    let tabs: Vec<Value> = ws
        .groups
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
                        "detached": t.detached,
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
    let collapsed: Vec<&str> = ws
        .groups
        .iter()
        .filter(|g| g.collapsed)
        .map(|g| g.head.id.as_str())
        .collect();
    json!({ "active": ws.active, "collapsed": collapsed, "tabs": tabs })
}

#[cfg(test)]
mod tests {
    use super::*;
    use flotilla_core::model::Opened;
    use serde_json::json;
    use url::Url;

    fn url(s: &str) -> Url {
        Url::parse(s).unwrap()
    }

    #[test]
    fn sidebar_dto_orders_groups_and_marks_heads() {
        let mut ws = Workspace::default();
        let Opened::Added(h1) =
            ws.new_group(&url("https://drive.google.com/drive/folders/f1"), "default")
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
        let Opened::Added(h2) =
            ws.new_group(&url("https://drive.google.com/drive/folders/f2"), "default")
        else {
            panic!()
        };
        ws.set_title(&h2, "Website");
        ws.set_collapsed(&h1, true);
        ws.set_detached(&m1, true);

        let dto = sidebar_dto(&ws, &HashSet::from([h2.clone()]));
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
    fn empty_workspace_has_no_rows_and_no_active() {
        let dto = sidebar_dto(&Workspace::default(), &HashSet::new());
        assert_eq!(dto["active"], Value::Null);
        assert_eq!(dto["collapsed"], json!([]));
        assert_eq!(dto["tabs"], json!([]));
    }
}
