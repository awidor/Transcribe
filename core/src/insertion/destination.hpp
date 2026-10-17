#pragma once

#include <algorithm>
#include <array>
#include <optional>
#include <string_view>

// Recognizes focused elements that clearly cannot take typed text, so the
// transcript is offered for dragging instead of pasted into nothing.
// Anything uncertain still receives the paste.
namespace transcribe::insertion::destination {

inline constexpr const char* NO_TEXT_FIELD = "No text field focused";

// UI Automation control type and, when a Value pattern exists, its read-only state.
inline bool windows_rejects(std::optional<int> control_type, std::optional<bool> read_only) {
    constexpr int EDIT = 50004;
    constexpr int DOCUMENT = 50030;
    constexpr std::array NOT_TEXT{
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
    };
    // A writable value takes text, including spreadsheet cells.
    if (read_only == false || !control_type) return false;
    // A page with no field selected is a read-only document.
    if (read_only == true && (*control_type == EDIT || *control_type == DOCUMENT)) return true;
    return std::ranges::find(NOT_TEXT, *control_type) != NOT_TEXT.end();
}

// AT-SPI role name and its editable state.
inline bool linux_rejects(std::string_view role, bool editable) {
    constexpr std::array<std::string_view, 34> NOT_TEXT{
        "push button",
        "toggle button",
        "check box",
        "radio button",
        "combo box",
        "menu",
        "menu bar",
        "menu item",
        "check menu item",
        "radio menu item",
        "list",
        "list box",
        "list item",
        "tree",
        "tree item",
        "tree table",
        "table",
        "table cell",
        "page tab",
        "page tab list",
        "link",
        "image",
        "icon",
        "slider",
        "scroll bar",
        "tool bar",
        "label",
        "static",
        "heading",
        "status bar",
        "progress bar",
        "desktop frame",
        "desktop icon",
        // A page with no field selected.
        "document web",
    };
    return !editable && std::ranges::find(NOT_TEXT, role) != NOT_TEXT.end();
}

} // namespace transcribe::insertion::destination
