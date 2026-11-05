//! Recognizes focused elements that clearly cannot take typed text, so the
//! transcript is offered for dragging instead of pasted into nothing.
//! Anything uncertain still receives the paste.

pub(crate) const NO_TEXT_FIELD: &str = "No text field focused";

/// UI Automation control type and, when a Value pattern exists, its read-only state.
#[cfg(any(windows, test))]
pub(crate) fn windows_rejects(control_type: Option<i32>, read_only: Option<bool>) -> bool {
    const EDIT: i32 = 50004;
    const DOCUMENT: i32 = 50030;
    const NOT_TEXT: [i32; 31] = [
        50000, // Button
        50001, // Calendar
        50002, // CheckBox
        50003, // ComboBox
        50005, // Hyperlink
        50006, // Image
        50007, // ListItem
        50008, // List
        50009, // Menu
        50010, // MenuBar
        50011, // MenuItem
        50012, // ProgressBar
        50013, // RadioButton
        50014, // ScrollBar
        50015, // Slider
        50017, // StatusBar
        50018, // Tab
        50019, // TabItem
        50020, // Text
        50021, // ToolBar
        50022, // ToolTip
        50023, // Tree
        50024, // TreeItem
        50027, // Thumb
        50028, // DataGrid
        50029, // DataItem
        50031, // SplitButton
        50034, // Header
        50035, // HeaderItem
        50036, // Table
        50037, // TitleBar
    ];
    match (control_type, read_only) {
        // A writable value takes text, including spreadsheet cells.
        (_, Some(false)) => false,
        // A page with no field selected is a read-only document.
        (Some(EDIT | DOCUMENT), Some(true)) => true,
        (Some(control), _) => NOT_TEXT.contains(&control),
        (None, _) => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_rejects_only_clear_non_text_focus() {
        // Measured from Chromium: page body, fields, read-only input, bare contenteditable.
        assert!(windows_rejects(Some(50030), Some(true)));
        assert!(!windows_rejects(Some(50004), Some(false)));
        assert!(windows_rejects(Some(50004), Some(true)));
        assert!(!windows_rejects(Some(50026), None));
        // Button, link, desktop and Explorer items, tabs.
        for control in [50000, 50005, 50007, 50008, 50019] {
            assert!(windows_rejects(Some(control), None), "{control}");
        }
        // A spreadsheet cell with a writable value.
        assert!(!windows_rejects(Some(50029), Some(false)));
        // Editors without a Value pattern, custom panes, and unknown focus.
        assert!(!windows_rejects(Some(50030), None));
        assert!(!windows_rejects(Some(50004), None));
        assert!(!windows_rejects(Some(50033), None));
        assert!(!windows_rejects(Some(50025), Some(true)));
        assert!(!windows_rejects(None, None));
    }
}
