use super::*;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc::unbounded_channel;

#[test]
fn footer_hint_row_is_separated_from_composer() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );

    let area = Rect::new(0, 0, 40, 6);
    let mut buf = Buffer::empty(area);
    composer.render(area, &mut buf);

    let row_to_string = |y: u16| {
        let mut row = String::new();
        for x in 0..area.width {
            row.push(buf[(x, y)].symbol().chars().next().unwrap_or(' '));
        }
        row
    };

    let mut hint_row: Option<(u16, String)> = None;
    for y in 0..area.height {
        let row = row_to_string(y);
        if row.contains("? for shortcuts") {
            hint_row = Some((y, row));
            break;
        }
    }

    let (hint_row_idx, hint_row_contents) =
        hint_row.expect("expected footer hint row to be rendered");
    assert_eq!(
        hint_row_idx,
        area.height - 1,
        "hint row should occupy the bottom line: {hint_row_contents:?}",
    );

    assert!(
        hint_row_idx > 0,
        "expected a spacing row above the footer hints",
    );

    let spacing_row = row_to_string(hint_row_idx - 1);
    assert_eq!(
        spacing_row.trim(),
        "",
        "expected blank spacing row above hints but saw: {spacing_row:?}",
    );
}

#[test]
fn footer_flash_overrides_footer_hint_override() {
    let (tx, _rx) = unbounded_channel::<AppEvent>();
    let sender = AppEventSender::new(tx);
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        sender,
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ false,
    );
    composer.set_footer_hint_override(Some(vec![("K".to_string(), "label".to_string())]));
    composer.show_footer_flash(Line::from("FLASH"), Duration::from_secs(10));

    let area = Rect::new(0, 0, 60, 6);
    let mut buf = Buffer::empty(area);
    composer.render(area, &mut buf);

    let mut bottom_row = String::new();
    for x in 0..area.width {
        bottom_row.push(
            buf[(x, area.height - 1)]
                .symbol()
                .chars()
                .next()
                .unwrap_or(' '),
        );
    }
    assert!(
        bottom_row.contains("FLASH"),
        "expected flash content to render in footer row, saw: {bottom_row:?}",
    );
    assert!(
        !bottom_row.contains("K label"),
        "expected flash to override hint override, saw: {bottom_row:?}",
    );
}
