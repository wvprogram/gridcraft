//! Clicking a hyperlink in a received workbook only opens web and email links. `follow_link`
//! used to hand any target containing `://` to `explorer` / `open` / `xdg-open`, so a cell
//! linked to `file:///C:/Windows/System32/calc.exe` or a network share launched it on a click.

use std::sync::{Arc, Mutex};

use gridcraft_engine::core::CellRef;
use gridcraft_engine::model::Hyperlink;
use gridcraft_engine::{DocState, Session};
use gridcraft_ui_egui::{Services, SheetApp, grid};

/// A session holding a workbook (saved as .xlsx and read back, like a received file) whose
/// cells link to `targets`, and an app that records what it hands to the operating system.
fn app_with_links(targets: &[&str]) -> (SheetApp, Arc<Mutex<Vec<String>>>) {
    let mut wb = gridcraft_engine::model::Workbook::new();
    wb.sheets.push(Arc::new(gridcraft_engine::model::Sheet::new("Sheet2")));
    if let Some(sh) = wb.sheet_mut(0) {
        for (i, t) in targets.iter().enumerate() {
            sh.hyperlinks.insert(CellRef::new(i as u32, 0), Hyperlink { target: t.to_string(), tooltip: None });
        }
    }
    let bytes = gridcraft_engine::io::save_bytes(&wb, "links.xlsx").unwrap();
    let (wb, _) = gridcraft_engine::io::open_bytes("links.xlsx", &bytes).unwrap();
    let mut session = Session::new();
    session.add_document(DocState::new(wb, None, "links".into()));
    let opened = Arc::new(Mutex::new(Vec::new()));
    let capture = opened.clone();
    let services = Services { open_url: Some(Box::new(move |u: &str| capture.lock().unwrap().push(u.to_string()))), ..Default::default() };
    (SheetApp::new(session, services), opened)
}

fn link_at(app: &SheetApp, row: u32) -> String {
    let sh = app.session.active().unwrap().wb.sheet(0).unwrap();
    sh.hyperlinks.get(&CellRef::new(row, 0)).map(|h| h.target.clone()).unwrap_or_default()
}

#[test]
fn only_web_and_email_links_reach_the_operating_system() {
    let hostile = [
        "file:///C:/Windows/System32/calc.exe",
        "file://attacker.example/share/payload.exe",
        "\\\\attacker.example\\share\\payload.exe",
        "ms-msdt:/id PCWDiagnostic",
        "search-ms://query=invoice",
        "javascript:alert(1)",
    ];
    let (mut app, opened) = app_with_links(&hostile);
    for row in 0..hostile.len() as u32 {
        let target = link_at(&app, row);
        assert!(!target.is_empty(), "row {row}: the link survived the .xlsx round trip");
        app.toast = None;
        grid::follow_link(&mut app, &target);
        assert!(opened.lock().unwrap().is_empty(), "{target:?} was handed to the operating system");
        let toast = app.toast.as_ref().map(|t| t.0.clone()).unwrap_or_default();
        assert_eq!(toast, grid::BLOCKED_LINK, "{target:?}: the user is told why nothing happened");
    }
}

#[test]
fn web_email_and_in_workbook_links_still_work() {
    let (mut app, opened) = app_with_links(&["https://example.com/report", "mailto:team@example.com"]);
    for row in 0..2 {
        let target = link_at(&app, row);
        grid::follow_link(&mut app, &target);
    }
    assert_eq!(*opened.lock().unwrap(), ["https://example.com/report", "mailto:team@example.com"]);
    app.toast = None;
    grid::follow_link(&mut app, "#Sheet2!C7");
    grid::follow_link(&mut app, "Sheet2!B3:B4");
    assert!(app.toast.is_none(), "in-workbook links navigate quietly");
    assert_eq!(opened.lock().unwrap().len(), 2, "nothing more went to the operating system");
}
