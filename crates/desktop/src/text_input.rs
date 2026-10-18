// Adapted from GPUI examples/input.rs, Zed Industries (Apache-2.0).
// See licenses/gpui-input-Apache-2.0.txt.
use std::ops::Range;

use gpui::{
    App, Bounds, ClipboardItem, Context, CursorStyle, ElementId, ElementInputHandler, Entity,
    EntityInputHandler, FocusHandle, Focusable, GlobalElementId, KeyBinding, LayoutId, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, ShapedLine, SharedString, Style,
    TextRun, UTF16Selection, Window, actions, div, fill, hsla, point, prelude::*, px, relative,
    rgb, rgba, size,
};
use unicode_segmentation::*;

actions!(
    text_input,
    [
        Backspace,
        Delete,
        Left,
        Right,
        SelectLeft,
        SelectRight,
        SelectAll,
        Home,
        End,
        ShowCharacterPalette,
        Paste,
        Cut,
        Copy,
        NewLine,
        Up,
        Down,
    ]
);

pub struct Edited;
impl gpui::EventEmitter<Edited> for TextInput {}

pub struct TextInput {
    focus_handle: FocusHandle,
    content: SharedString,
    placeholder: SharedString,
    selected_range: Range<usize>,
    selection_reversed: bool,
    marked_range: Option<Range<usize>>,
    last_layout: Vec<(usize, ShapedLine)>,
    first_line: usize,
    scroll_x: Pixels,
    last_bounds: Option<Bounds<Pixels>>,
    is_selecting: bool,
}

impl TextInput {
    pub fn new(content: String, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            content: content.into(),
            placeholder: "IPv4, CIDR, or hostname — one per line".into(),
            selected_range: 0..0,
            selection_reversed: false,
            marked_range: None,
            last_layout: vec![],
            last_bounds: None,
            is_selecting: false,
            first_line: 0,
            scroll_x: px(0.),
        }
    }
    pub fn content(&self) -> String {
        self.content.to_string()
    }
    pub fn set_content(&mut self, content: String, cx: &mut Context<Self>) {
        self.content = content.into();
        self.selected_range = 0..0;
        self.marked_range = None;
        self.first_line = 0;
        self.scroll_x = px(0.);
        cx.notify();
    }
    fn ensure_visible(&mut self) {
        let row = self.content[..self.cursor_offset()]
            .bytes()
            .filter(|b| *b == b'\n')
            .count();
        if row < self.first_line {
            self.first_line = row;
        } else if row >= self.first_line + 12 {
            self.first_line = row.saturating_sub(11);
        }
    }
    fn new_line(&mut self, _: &NewLine, window: &mut Window, cx: &mut Context<Self>) {
        self.replace_text_in_range(None, "\n", window, cx);
    }
    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let start = self.content[..cursor].rfind('\n').map_or(0, |p| p + 1);
        if start > 0 {
            let previous = self.content[..start - 1].rfind('\n').map_or(0, |p| p + 1);
            let mut target = (previous + cursor - start).min(start - 1);
            while !self.content.is_char_boundary(target) {
                target -= 1;
            }
            self.move_to(target, cx);
        }
    }
    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        let cursor = self.cursor_offset();
        let start = self.content[..cursor].rfind('\n').map_or(0, |p| p + 1);
        if let Some(end) = self.content[cursor..].find('\n').map(|n| cursor + n) {
            let next_end = self.content[end + 1..]
                .find('\n')
                .map_or(self.content.len(), |p| end + 1 + p);
            let mut target = (end + 1 + cursor - start).min(next_end);
            while !self.content.is_char_boundary(target) {
                target -= 1;
            }
            self.move_to(target, cx);
        }
    }
    fn scroll(&mut self, event: &gpui::ScrollWheelEvent, _: &mut Window, cx: &mut Context<Self>) {
        let dy = match event.delta {
            gpui::ScrollDelta::Pixels(p) => f32::from(p.y) / 20.,
            gpui::ScrollDelta::Lines(p) => p.y,
        };
        let max = self
            .content
            .bytes()
            .filter(|b| *b == b'\n')
            .count()
            .saturating_sub(11);
        self.first_line =
            (self.first_line as isize - dy.round() as isize).clamp(0, max as isize) as usize;
        cx.notify();
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.previous_boundary(self.cursor_offset()), cx);
        } else {
            self.move_to(self.selected_range.start, cx)
        }
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            self.move_to(self.next_boundary(self.selected_range.end), cx);
        } else {
            self.move_to(self.selected_range.end, cx)
        }
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.previous_boundary(self.cursor_offset()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.select_to(self.next_boundary(self.cursor_offset()), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
        self.select_to(self.content.len(), cx)
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(0, cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.move_to(self.content.len(), cx);
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let prev = self.previous_boundary(self.cursor_offset());
            if self.cursor_offset() == prev {
                window.play_system_bell();
                return;
            }
            self.select_to(prev, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected_range.is_empty() {
            let next = self.next_boundary(self.cursor_offset());
            if self.cursor_offset() == next {
                window.play_system_bell();
                return;
            }
            self.select_to(next, cx)
        }
        self.replace_text_in_range(None, "", window, cx)
    }

    fn on_mouse_down(
        &mut self,
        event: &MouseDownEvent,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.is_selecting = true;

        if event.modifiers.shift {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        } else {
            self.move_to(self.index_for_mouse_position(event.position), cx)
        }
    }

    fn on_mouse_up(&mut self, _: &MouseUpEvent, _window: &mut Window, _: &mut Context<Self>) {
        self.is_selecting = false;
    }

    fn on_mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.is_selecting {
            self.select_to(self.index_for_mouse_position(event.position), cx);
        }
    }

    fn show_character_palette(
        &mut self,
        _: &ShowCharacterPalette,
        window: &mut Window,
        _: &mut Context<Self>,
    ) {
        window.show_character_palette();
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            self.replace_text_in_range(None, &text.replace("\r\n", "\n"), window, cx);
        }
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
        }
    }
    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected_range.is_empty() {
            cx.write_to_clipboard(ClipboardItem::new_string(
                self.content[self.selected_range.clone()].to_string(),
            ));
            self.replace_text_in_range(None, "", window, cx)
        }
    }

    fn move_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        self.selected_range = offset..offset;
        self.ensure_visible();
        cx.notify()
    }

    fn cursor_offset(&self) -> usize {
        if self.selection_reversed {
            self.selected_range.start
        } else {
            self.selected_range.end
        }
    }

    fn index_for_mouse_position(&self, position: Point<Pixels>) -> usize {
        let Some(bounds) = self.last_bounds else {
            return 0;
        };
        let row = (f32::from(position.y - bounds.top()) / 20.).max(0.) as usize;
        let Some((start, line)) = self
            .last_layout
            .get(row.min(self.last_layout.len().saturating_sub(1)))
        else {
            return 0;
        };
        (start + line.closest_index_for_x(position.x - bounds.left() + self.scroll_x))
            .min(self.content.len())
    }

    fn select_to(&mut self, offset: usize, cx: &mut Context<Self>) {
        if self.selection_reversed {
            self.selected_range.start = offset
        } else {
            self.selected_range.end = offset
        };
        if self.selected_range.end < self.selected_range.start {
            self.selection_reversed = !self.selection_reversed;
            self.selected_range = self.selected_range.end..self.selected_range.start;
        }
        self.ensure_visible();
        cx.notify()
    }

    fn offset_from_utf16(&self, offset: usize) -> usize {
        let mut utf8_offset = 0;
        let mut utf16_count = 0;

        for ch in self.content.chars() {
            if utf16_count >= offset {
                break;
            }
            utf16_count += ch.len_utf16();
            utf8_offset += ch.len_utf8();
        }

        utf8_offset
    }

    fn offset_to_utf16(&self, offset: usize) -> usize {
        let mut utf16_offset = 0;
        let mut utf8_count = 0;

        for ch in self.content.chars() {
            if utf8_count >= offset {
                break;
            }
            utf8_count += ch.len_utf8();
            utf16_offset += ch.len_utf16();
        }

        utf16_offset
    }

    fn range_to_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.offset_to_utf16(range.start)..self.offset_to_utf16(range.end)
    }

    fn range_from_utf16(&self, range_utf16: &Range<usize>) -> Range<usize> {
        self.offset_from_utf16(range_utf16.start)..self.offset_from_utf16(range_utf16.end)
    }

    fn previous_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .rev()
            .find_map(|(idx, _)| (idx < offset).then_some(idx))
            .unwrap_or(0)
    }

    fn next_boundary(&self, offset: usize) -> usize {
        self.content
            .grapheme_indices(true)
            .find_map(|(idx, _)| (idx > offset).then_some(idx))
            .unwrap_or(self.content.len())
    }
}

impl EntityInputHandler for TextInput {
    fn text_for_range(
        &mut self,
        range_utf16: Range<usize>,
        actual_range: &mut Option<Range<usize>>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_from_utf16(&range_utf16);
        actual_range.replace(self.range_to_utf16(&range));
        Some(self.content[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _ignore_disabled_input: bool,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_to_utf16(&self.selected_range),
            reversed: self.selection_reversed,
        })
    }

    fn marked_text_range(
        &self,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Range<usize>> {
        self.marked_range
            .as_ref()
            .map(|range| self.range_to_utf16(range))
    }

    fn unmark_text(&mut self, _window: &mut Window, _cx: &mut Context<Self>) {
        self.marked_range = None;
    }

    fn replace_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        let Some(content) = edited(&self.content, range.clone(), new_text) else {
            return;
        };
        self.content = content.into();
        cx.emit(Edited);
        self.selected_range = range.start + new_text.len()..range.start + new_text.len();
        self.marked_range.take();
        self.ensure_visible();
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range_utf16: Option<Range<usize>>,
        new_text: &str,
        new_selected_range_utf16: Option<Range<usize>>,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range_utf16
            .as_ref()
            .map(|range_utf16| self.range_from_utf16(range_utf16))
            .or(self.marked_range.clone())
            .unwrap_or(self.selected_range.clone());

        let Some(content) = edited(&self.content, range.clone(), new_text) else {
            return;
        };
        self.content = content.into();
        cx.emit(Edited);
        if !new_text.is_empty() {
            self.marked_range = Some(range.start..range.start + new_text.len());
        } else {
            self.marked_range = None;
        }
        self.selected_range = new_selected_range_utf16
            .map(|r| {
                let start = utf16_index(new_text, r.start);
                let end = utf16_index(new_text, r.end);
                range.start + start.min(end)..range.start + start.max(end)
            })
            .unwrap_or_else(|| range.start + new_text.len()..range.start + new_text.len());
        self.ensure_visible();
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range_utf16: Range<usize>,
        bounds: Bounds<Pixels>,
        _window: &mut Window,
        _cx: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let range = self.range_from_utf16(&range_utf16);
        let (row, (start, line)) =
            self.last_layout
                .iter()
                .enumerate()
                .find(|(_, (start, line))| {
                    range.start >= *start && range.start <= *start + line.text.len()
                })?;
        let y = bounds.top() + px(row as f32 * 20.);
        Some(Bounds::from_corners(
            point(
                bounds.left() + line.x_for_index(range.start - start) - self.scroll_x,
                y,
            ),
            point(
                bounds.left() + line.x_for_index((range.end - start).min(line.text.len()))
                    - self.scroll_x,
                y + px(20.),
            ),
        ))
    }
    fn character_index_for_point(
        &mut self,
        position: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.offset_to_utf16(self.index_for_mouse_position(position)))
    }
}

struct TextElement {
    input: Entity<TextInput>,
}

struct PrepaintState {
    lines: Vec<(usize, ShapedLine)>,
}

impl IntoElement for TextElement {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

impl Element for TextElement {
    type RequestLayoutState = ();
    type PrepaintState = PrepaintState;

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.).into();
        style.size.height = px(240.).into();
        (window.request_layout(style, [], cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        let input = self.input.read(cx);
        let style = window.text_style();
        let mut offset = 0;
        let mut lines = vec![];
        for (row, text) in input.content.split('\n').enumerate() {
            if row >= input.first_line && lines.len() < 12 {
                let run = TextRun {
                    len: text.len(),
                    font: style.font(),
                    color: style.color,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line = window.text_system().shape_line(
                    text.to_owned().into(),
                    style.font_size.to_pixels(window.rem_size()),
                    &[run],
                    None,
                );
                lines.push((offset, line));
            }
            offset += text.len() + 1;
            if lines.len() == 12 {
                break;
            }
        }
        if input.content.is_empty() {
            let text = input.placeholder.clone();
            let run = TextRun {
                len: text.len(),
                font: style.font(),
                color: hsla(0., 0., 0.55, 1.),
                background_color: None,
                underline: None,
                strikethrough: None,
            };
            lines = vec![(
                0,
                window.text_system().shape_line(
                    text,
                    style.font_size.to_pixels(window.rem_size()),
                    &[run],
                    None,
                ),
            )];
        }
        let _ = bounds;
        PrepaintState { lines }
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&gpui::InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let input = self.input.read(cx);
        let focus_handle = input.focus_handle.clone();
        let selection = input.selected_range.clone();
        let cursor = input.cursor_offset();
        let empty = input.content.is_empty();
        let mut scroll_x = input.scroll_x;
        if let Some((start, line)) = prepaint
            .lines
            .iter()
            .find(|(start, line)| cursor >= *start && cursor <= *start + line.text.len())
        {
            let x = if empty {
                px(0.)
            } else {
                line.x_for_index(cursor - start)
            };
            if x - scroll_x > bounds.size.width - px(8.) {
                scroll_x = x - bounds.size.width + px(8.);
            }
            if x < scroll_x {
                scroll_x = x;
            }
        }
        window.handle_input(
            &focus_handle,
            ElementInputHandler::new(bounds, self.input.clone()),
            cx,
        );
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            for (row, (start, line)) in prepaint.lines.iter().enumerate() {
                let origin = point(
                    bounds.left() - scroll_x,
                    bounds.top() + px(row as f32 * 20.),
                );
                let end = start + line.text.len();
                if !empty && selection.start < end && selection.end > *start {
                    let left = line
                        .x_for_index(selection.start.saturating_sub(*start).min(line.text.len()));
                    let right =
                        line.x_for_index(selection.end.saturating_sub(*start).min(line.text.len()));
                    window.paint_quad(fill(
                        Bounds::from_corners(
                            point(origin.x + left, origin.y),
                            point(origin.x + right, origin.y + px(20.)),
                        ),
                        rgba(0x3976bd40),
                    ));
                }
                let _ = line.paint(origin, px(20.), gpui::TextAlign::Left, None, window, cx);
                if focus_handle.is_focused(window) && cursor >= *start && cursor <= end {
                    let x = if empty {
                        px(0.)
                    } else {
                        line.x_for_index(cursor - start)
                    };
                    window.paint_quad(fill(
                        Bounds::new(point(origin.x + x, origin.y), size(px(1.5), px(20.))),
                        rgb(0x0866d9),
                    ));
                }
            }
        });
        self.input.update(cx, |input, _| {
            input.last_layout = std::mem::take(&mut prepaint.lines);
            input.last_bounds = Some(bounds);
            input.scroll_x = scroll_x;
        });
    }
}

impl Render for TextInput {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let entity = cx.entity();
        div().id("destinations-editor").role(accesskit::Role::MultilineTextInput)
            .aria_label("VPN destinations").aria_description("IPv4, CIDR or explicit hostname, one per line. Wildcards and IPv6 are unsupported.")
            .aria_value(self.content.clone()).aria_placeholder(self.placeholder.clone())
            .on_a11y_action(accesskit::Action::SetValue,move|data,_,cx|{if let Some(accesskit::ActionData::Value(text))=data && edited("",0..0,text).is_some(){entity.update(cx,|input,cx|{input.set_content(text.to_string(),cx);cx.emit(Edited);});}})
            .flex()
            .key_context("TextInput")
            .track_focus(&self.focus_handle(cx))
            .cursor(CursorStyle::IBeam)
            .on_action(cx.listener(Self::new_line)).on_action(cx.listener(Self::up)).on_action(cx.listener(Self::down))
            .on_scroll_wheel(cx.listener(Self::scroll))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::show_character_palette))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::copy))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
            .on_mouse_move(cx.listener(Self::on_mouse_move))
            .w_full().border_1().rounded_lg().border_color(rgba(0x88888833))
            .focus_visible(|style|style.border_color(rgb(0x0866d9)))
            .line_height(px(20.)).text_size(px(14.))
            .child(div().h(px(252.)).w_full().p(px(6.)).child(TextElement{input:cx.entity()}))
    }
}

impl Focusable for TextInput {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

pub fn bind(cx: &mut App) {
    cx.bind_keys([
        KeyBinding::new("backspace", Backspace, Some("TextInput")),
        KeyBinding::new("delete", Delete, Some("TextInput")),
        KeyBinding::new("left", Left, Some("TextInput")),
        KeyBinding::new("right", Right, Some("TextInput")),
        KeyBinding::new("up", Up, Some("TextInput")),
        KeyBinding::new("down", Down, Some("TextInput")),
        KeyBinding::new("enter", NewLine, Some("TextInput")),
        KeyBinding::new("shift-left", SelectLeft, Some("TextInput")),
        KeyBinding::new("shift-right", SelectRight, Some("TextInput")),
        KeyBinding::new("cmd-a", SelectAll, Some("TextInput")),
        KeyBinding::new("cmd-v", Paste, Some("TextInput")),
        KeyBinding::new("cmd-c", Copy, Some("TextInput")),
        KeyBinding::new("cmd-x", Cut, Some("TextInput")),
        KeyBinding::new("ctrl-a", SelectAll, Some("TextInput")),
        KeyBinding::new("ctrl-v", Paste, Some("TextInput")),
        KeyBinding::new("ctrl-c", Copy, Some("TextInput")),
        KeyBinding::new("ctrl-x", Cut, Some("TextInput")),
        KeyBinding::new("home", Home, Some("TextInput")),
        KeyBinding::new("end", End, Some("TextInput")),
        KeyBinding::new("ctrl-cmd-space", ShowCharacterPalette, Some("TextInput")),
    ]);
}
fn utf16_index(text: &str, offset: usize) -> usize {
    let (mut bytes, mut units) = (0, 0);
    for ch in text.chars() {
        if units >= offset {
            break;
        }
        bytes += ch.len_utf8();
        units += ch.len_utf16();
    }
    bytes
}
fn edited(content: &str, range: Range<usize>, text: &str) -> Option<String> {
    if range.start > range.end
        || range.end > content.len()
        || !content.is_char_boundary(range.start)
        || !content.is_char_boundary(range.end)
    {
        return None;
    }
    let length = content
        .len()
        .checked_sub(range.len())?
        .checked_add(text.len())?;
    if length > 1_048_576 {
        return None;
    }
    let result = content[..range.start].to_owned() + text + &content[range.end..];
    if result.lines().any(|line| line.len() > 1024) {
        return None;
    }
    Some(result)
}
#[cfg(test)]
mod tests {
    #[test]
    fn multiline_edit_respects_unicode_boundaries_and_size_limit() {
        assert_eq!(
            super::edited("one\n東京", 4..10, "two").unwrap(),
            "one\ntwo"
        );
        assert!(super::edited("東京", 1..3, "").is_none());
        assert!(super::edited("abc", std::ops::Range { start: 2, end: 1 }, "x").is_none());
        assert!(super::edited("", 0..0, &"x".repeat(1_048_577)).is_none());
    }
}
