use std::cell::{Cell, RefCell};
use std::ops::Deref;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use smol_str::SmolStr;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use web_sys::{
    ClipboardEvent, CompositionEvent, CssStyleDeclaration, Document, Event, EventTarget,
    FocusEvent, HtmlCanvasElement, HtmlElement, HtmlInputElement, HtmlTextAreaElement, InputEvent,
    KeyboardEvent, PointerEvent, WheelEvent,
};

use crate::dpi::{LogicalPosition, PhysicalPosition, PhysicalSize};
use crate::error::OsError as RootOE;
use crate::event::{
    Force, Ime, InnerSizeWriter, MouseButton, MouseScrollDelta, WebClipboardAction,
    WebClipboardEvent, WebSelectionDirection, WebTextInputEvent,
};
use crate::keyboard::{Key, KeyLocation, ModifiersState, PhysicalKey};
use crate::platform::web::BrowserDefaults;
use crate::platform_impl::OsError;
use crate::window::{WindowAttributes, WindowId as RootWindowId};

use super::super::cursor::CursorHandler;
use super::super::main_thread::MainThreadMarker;
use super::super::WindowId;
use super::animation_frame::AnimationFrameHandler;
use super::event_handle::EventListenerHandle;
use super::intersection_handle::IntersectionObserverHandle;
use super::media_query_handle::MediaQueryListHandle;
use super::pointer::PointerHandler;
use super::{event, fullscreen, ButtonsState, ResizeScaleHandle};

#[allow(dead_code)]
pub struct Canvas {
    common: Common,
    id: WindowId,
    pub has_focus: Rc<Cell<bool>>,
    pub prevent_default: Rc<Cell<bool>>,
    pub browser_defaults: Rc<Cell<BrowserDefaults>>,
    pub is_intersecting: Option<bool>,
    on_touch_start: Option<EventListenerHandle<dyn FnMut(Event)>>,
    on_focus: Option<EventListenerHandle<dyn FnMut(FocusEvent)>>,
    on_blur: Option<EventListenerHandle<dyn FnMut(FocusEvent)>>,
    on_keyboard_release: Option<EventListenerHandle<dyn FnMut(KeyboardEvent)>>,
    on_keyboard_press: Option<EventListenerHandle<dyn FnMut(KeyboardEvent)>>,
    on_ime_keyboard_release: Vec<EventListenerHandle<dyn FnMut(KeyboardEvent)>>,
    on_ime_keyboard_press: Vec<EventListenerHandle<dyn FnMut(KeyboardEvent)>>,
    on_mouse_wheel: Option<EventListenerHandle<dyn FnMut(WheelEvent)>>,
    on_dark_mode: Option<MediaQueryListHandle>,
    pointer_handler: PointerHandler,
    on_resize_scale: Option<ResizeScaleHandle>,
    on_intersect: Option<IntersectionObserverHandle>,
    animation_frame_handler: AnimationFrameHandler,
    on_touch_end: Option<EventListenerHandle<dyn FnMut(Event)>>,
    on_context_menu: Option<EventListenerHandle<dyn FnMut(PointerEvent)>>,
    on_copy: Option<EventListenerHandle<dyn FnMut(ClipboardEvent)>>,
    on_cut: Option<EventListenerHandle<dyn FnMut(ClipboardEvent)>>,
    on_paste: Option<EventListenerHandle<dyn FnMut(ClipboardEvent)>>,
    ime_element: ImeElement,
    ime_allowed: Rc<Cell<bool>>,
    on_ime_focus: Vec<EventListenerHandle<dyn FnMut(FocusEvent)>>,
    on_ime_blur: Vec<EventListenerHandle<dyn FnMut(FocusEvent)>>,
    on_composition_start: Vec<EventListenerHandle<dyn FnMut(CompositionEvent)>>,
    on_composition_update: Vec<EventListenerHandle<dyn FnMut(CompositionEvent)>>,
    on_composition_end: Vec<EventListenerHandle<dyn FnMut(CompositionEvent)>>,
    on_before_input: Vec<EventListenerHandle<dyn FnMut(InputEvent)>>,
    on_text_input: Vec<EventListenerHandle<dyn FnMut(InputEvent)>>,
    pub cursor: CursorHandler,
}

#[derive(Clone)]
pub(super) struct ImeElement {
    textarea: HtmlTextAreaElement,
    password: HtmlInputElement,
    password_active: Rc<Cell<bool>>,
}

impl ImeElement {
    fn create(document: &Document) -> Result<Self, RootOE> {
        let textarea: HtmlTextAreaElement = document
            .create_element("textarea")
            .map_err(|_| os_error!(OsError("Failed to create IME textarea".to_owned())))?
            .unchecked_into();
        let password: HtmlInputElement = document
            .create_element("input")
            .map_err(|_| os_error!(OsError("Failed to create IME password input".to_owned())))?
            .unchecked_into();
        password.set_type("password");
        let this = Self { textarea, password, password_active: Rc::new(Cell::new(false)) };
        for element in this.html_elements() {
            element.set_tab_index(-1);
            element.set_attribute("autocomplete", "off").ok();
            element.set_attribute("autocapitalize", "off").ok();
            let style = element.style();
            style.set_property("position", "fixed").ok();
            style.set_property("width", "1px").ok();
            style.set_property("height", "1px").ok();
            style.set_property("opacity", "0").ok();
            style.set_property("pointer-events", "none").ok();
            style.set_property("resize", "none").ok();
            style.set_property("overflow", "hidden").ok();
            document
                .body()
                .expect("Failed to get body from document")
                .append_child(&element)
                .map_err(|_| os_error!(OsError("Failed to append IME element".to_owned())))?;
        }
        Ok(this)
    }

    fn html_elements(&self) -> [HtmlElement; 2] {
        [self.textarea.clone().unchecked_into(), self.password.clone().unchecked_into()]
    }

    fn event_targets(&self) -> [EventTarget; 2] {
        [self.textarea.clone().into(), self.password.clone().into()]
    }

    fn active_html_element(&self) -> HtmlElement {
        if self.password_active.get() {
            self.password.clone().unchecked_into()
        } else {
            self.textarea.clone().unchecked_into()
        }
    }

    fn inactive_html_element(&self) -> HtmlElement {
        if self.password_active.get() {
            self.textarea.clone().unchecked_into()
        } else {
            self.password.clone().unchecked_into()
        }
    }

    fn contains_target(&self, target: &EventTarget) -> bool {
        self.event_targets().iter().any(|candidate| candidate == target)
    }

    pub(super) fn focus(&self) {
        let _ = self.active_html_element().focus();
    }

    fn style(&self) -> CssStyleDeclaration {
        self.active_html_element().style()
    }

    fn set_password_active(&self, active: bool) {
        if self.password_active.get() == active {
            return;
        }
        let was_focused = self
            .common_document()
            .active_element()
            .is_some_and(|target| self.contains_target(&target.into()));
        self.textarea.set_value("");
        self.password.set_value("");
        self.password_active.set(active);
        if was_focused {
            self.focus();
        }
    }

    fn common_document(&self) -> Document {
        self.textarea.owner_document().expect("IME element must have an owner document")
    }

    fn value(&self) -> String {
        if self.password_active.get() {
            self.password.value()
        } else {
            self.textarea.value()
        }
    }

    fn set_value(&self, value: &str) {
        if self.password_active.get() {
            self.password.set_value(value);
            self.textarea.set_value("");
        } else {
            self.textarea.set_value(value);
            self.password.set_value("");
        }
    }

    fn selection_start(&self) -> Option<u32> {
        if self.password_active.get() {
            self.password.selection_start().ok().flatten()
        } else {
            self.textarea.selection_start().ok().flatten()
        }
    }

    fn selection_end(&self) -> Option<u32> {
        if self.password_active.get() {
            self.password.selection_end().ok().flatten()
        } else {
            self.textarea.selection_end().ok().flatten()
        }
    }

    fn selection_direction(&self) -> Option<String> {
        if self.password_active.get() {
            self.password.selection_direction().ok().flatten()
        } else {
            self.textarea.selection_direction().ok().flatten()
        }
    }

    fn set_selection_range(&self, start: u32, end: u32, direction: &str) {
        if self.password_active.get() {
            let _ = self.password.set_selection_range_with_direction(start, end, direction);
        } else {
            let _ = self.textarea.set_selection_range_with_direction(start, end, direction);
        }
    }

    fn set_attribute(&self, name: &str, value: &str) {
        let _ = self.active_html_element().set_attribute(name, value);
        let _ = self.inactive_html_element().remove_attribute(name);
    }

    fn remove_attribute(&self, name: &str) {
        for element in self.html_elements() {
            let _ = element.remove_attribute(name);
        }
    }

    fn remove(&self) {
        self.textarea.remove();
        self.password.remove();
    }
}

pub struct Common {
    pub window: web_sys::Window,
    pub document: Document,
    /// Note: resizing the HTMLCanvasElement should go through `backend::set_canvas_size` to ensure
    /// the DPI factor is maintained. Note: this is read-only because we use a pointer to this
    /// for [`WindowHandle`][rwh_06::WindowHandle].
    raw: Rc<HtmlCanvasElement>,
    style: Style,
    old_size: Rc<Cell<PhysicalSize<u32>>>,
    current_size: Rc<Cell<PhysicalSize<u32>>>,
}

#[derive(Clone, Debug)]
pub struct Style {
    read: CssStyleDeclaration,
    write: CssStyleDeclaration,
}

impl Canvas {
    pub(crate) fn create(
        main_thread: MainThreadMarker,
        id: WindowId,
        window: web_sys::Window,
        document: Document,
        attr: &mut WindowAttributes,
    ) -> Result<Self, RootOE> {
        let canvas = match attr.platform_specific.canvas.take().map(|canvas| {
            Arc::try_unwrap(canvas)
                .map(|canvas| canvas.into_inner(main_thread))
                .unwrap_or_else(|canvas| canvas.get(main_thread).clone())
        }) {
            Some(canvas) => canvas,
            None => document
                .create_element("canvas")
                .map_err(|_| os_error!(OsError("Failed to create canvas element".to_owned())))?
                .unchecked_into(),
        };

        if attr.platform_specific.append && !document.contains(Some(&canvas)) {
            document
                .body()
                .expect("Failed to get body from document")
                .append_child(&canvas)
                .expect("Failed to append canvas to body");
        }

        // A tabindex is needed in order to capture local keyboard events.
        // A "0" value means that the element should be focusable in
        // sequential keyboard navigation, but its order is defined by the
        // document's source order.
        // https://developer.mozilla.org/en-US/docs/Web/HTML/Global_attributes/tabindex
        if attr.platform_specific.focusable {
            canvas
                .set_attribute("tabindex", "0")
                .map_err(|_| os_error!(OsError("Failed to set a tabindex".to_owned())))?;
        }

        let style = Style::new(&window, &canvas);

        let ime_element = ImeElement::create(&document)?;

        let cursor = CursorHandler::new(main_thread, canvas.clone(), style.clone());

        let common = Common {
            window: window.clone(),
            document: document.clone(),
            raw: Rc::new(canvas.clone()),
            style,
            old_size: Rc::default(),
            current_size: Rc::default(),
        };

        if let Some(size) = attr.inner_size {
            let size = size.to_logical(super::scale_factor(&common.window));
            super::set_canvas_size(&common.document, &common.raw, &common.style, size);
        }

        if let Some(size) = attr.min_inner_size {
            let size = size.to_logical(super::scale_factor(&common.window));
            super::set_canvas_min_size(&common.document, &common.raw, &common.style, Some(size));
        }

        if let Some(size) = attr.max_inner_size {
            let size = size.to_logical(super::scale_factor(&common.window));
            super::set_canvas_max_size(&common.document, &common.raw, &common.style, Some(size));
        }

        if let Some(position) = attr.position {
            let position = position.to_logical(super::scale_factor(&common.window));
            super::set_canvas_position(&common.document, &common.raw, &common.style, position);
        }

        if attr.fullscreen.is_some() {
            fullscreen::request_fullscreen(&document, &canvas);
        }

        if attr.active {
            let _ = common.raw.focus();
        }

        Ok(Canvas {
            common,
            id,
            has_focus: Rc::new(Cell::new(false)),
            prevent_default: Rc::new(Cell::new(attr.platform_specific.prevent_default)),
            browser_defaults: Rc::new(Cell::new(attr.platform_specific.browser_defaults)),
            is_intersecting: None,
            on_touch_start: None,
            on_blur: None,
            on_focus: None,
            on_keyboard_release: None,
            on_keyboard_press: None,
            on_ime_keyboard_release: Vec::new(),
            on_ime_keyboard_press: Vec::new(),
            on_mouse_wheel: None,
            on_dark_mode: None,
            pointer_handler: PointerHandler::new(),
            on_resize_scale: None,
            on_intersect: None,
            animation_frame_handler: AnimationFrameHandler::new(window),
            on_touch_end: None,
            on_context_menu: None,
            on_copy: None,
            on_cut: None,
            on_paste: None,
            ime_element,
            ime_allowed: Rc::new(Cell::new(false)),
            on_ime_focus: Vec::new(),
            on_ime_blur: Vec::new(),
            on_composition_start: Vec::new(),
            on_composition_update: Vec::new(),
            on_composition_end: Vec::new(),
            on_before_input: Vec::new(),
            on_text_input: Vec::new(),
            cursor,
        })
    }

    pub fn set_cursor_lock(&self, lock: bool) -> Result<(), RootOE> {
        if lock {
            self.raw().request_pointer_lock();
        } else {
            self.common.document.exit_pointer_lock();
        }
        Ok(())
    }

    pub fn set_attribute(&self, attribute: &str, value: &str) {
        self.common
            .raw
            .set_attribute(attribute, value)
            .unwrap_or_else(|err| panic!("error: {err:?}\nSet attribute: {attribute}"))
    }

    pub fn position(&self) -> LogicalPosition<f64> {
        let bounds = self.common.raw.get_bounding_client_rect();
        let mut position = LogicalPosition { x: bounds.x(), y: bounds.y() };

        if self.document().contains(Some(self.raw())) && self.style().get("display") != "none" {
            position.x += super::style_size_property(self.style(), "border-left-width")
                + super::style_size_property(self.style(), "padding-left");
            position.y += super::style_size_property(self.style(), "border-top-width")
                + super::style_size_property(self.style(), "padding-top");
        }

        position
    }

    #[inline]
    pub fn old_size(&self) -> PhysicalSize<u32> {
        self.common.old_size.get()
    }

    #[inline]
    pub fn inner_size(&self) -> PhysicalSize<u32> {
        self.common.current_size.get()
    }

    #[inline]
    pub fn set_old_size(&self, size: PhysicalSize<u32>) {
        self.common.old_size.set(size)
    }

    #[inline]
    pub fn set_current_size(&self, size: PhysicalSize<u32>) {
        self.common.current_size.set(size)
    }

    #[inline]
    pub fn window(&self) -> &web_sys::Window {
        &self.common.window
    }

    #[inline]
    pub fn document(&self) -> &Document {
        &self.common.document
    }

    #[inline]
    pub fn raw(&self) -> &HtmlCanvasElement {
        &self.common.raw
    }

    #[inline]
    pub fn style(&self) -> &Style {
        &self.common.style
    }

    pub fn on_touch_start(&mut self) {
        let prevent_default = Rc::clone(&self.prevent_default);
        let browser_defaults = Rc::clone(&self.browser_defaults);
        self.on_touch_start = Some(self.common.add_event("touchstart", move |event: Event| {
            if prevent_default.get() && !browser_defaults.get().contains(BrowserDefaults::TOUCH) {
                event.prevent_default();
            }
        }));
    }

    pub fn on_blur<F>(&mut self, mut handler: F)
    where
        F: 'static + FnMut(),
    {
        let ime_element = self.ime_element.clone();
        self.on_blur = Some(self.common.add_event("blur", move |event: FocusEvent| {
            if !event
                .related_target()
                .as_ref()
                .is_some_and(|target| ime_element.contains_target(target))
            {
                handler();
            }
        }));
    }

    pub fn on_ime<F, I>(&mut self, focus_handler: F, ime_handler: I)
    where
        F: 'static + FnMut(bool),
        I: 'static + FnMut(Ime),
    {
        let canvas: EventTarget = self.common.raw().clone().into();
        let ime_handler = Rc::new(std::cell::RefCell::new(ime_handler));
        let focus_handler = Rc::new(std::cell::RefCell::new(focus_handler));

        let handler = Rc::clone(&ime_handler);
        let focus = Rc::clone(&focus_handler);
        self.on_ime_focus = self.add_ime_events("focus", move |_: FocusEvent| {
            focus.borrow_mut()(true);
            handler.borrow_mut()(Ime::Enabled);
        });

        let handler = Rc::clone(&ime_handler);
        let focus = focus_handler;
        let ime_element = self.ime_element.clone();
        self.on_ime_blur = self.add_ime_events("blur", move |event: FocusEvent| {
            if event
                .related_target()
                .as_ref()
                .is_some_and(|target| ime_element.contains_target(target))
            {
                return;
            }
            handler.borrow_mut()(Ime::Disabled);
            if event.related_target().as_ref() != Some(&canvas) {
                focus.borrow_mut()(false);
            }
        });

        let handler = Rc::clone(&ime_handler);
        self.on_composition_start =
            self.add_ime_events("compositionstart", move |event: CompositionEvent| {
                let text = event.data().unwrap_or_default();
                let end = text.len();
                handler.borrow_mut()(Ime::Preedit(text, Some((end, end))));
            });

        let handler = Rc::clone(&ime_handler);
        self.on_composition_update =
            self.add_ime_events("compositionupdate", move |event: CompositionEvent| {
                let text = event.data().unwrap_or_default();
                let end = text.len();
                handler.borrow_mut()(Ime::Preedit(text, Some((end, end))));
            });

        let handler = Rc::clone(&ime_handler);
        self.on_composition_end =
            self.add_ime_events("compositionend", move |_event: CompositionEvent| {
                handler.borrow_mut()(Ime::Preedit(String::new(), None));
            });
    }

    pub fn on_text_input<F>(&mut self, handler: F)
    where
        F: 'static + FnMut(WebTextInputEvent),
    {
        #[derive(Default)]
        struct BeforeInput {
            input_type: String,
            data: Option<String>,
            cancelable: bool,
        }

        let element = self.ime_element.clone();
        let pending = Rc::new(RefCell::new(BeforeInput::default()));
        let before = Rc::clone(&pending);
        self.on_before_input = self.add_ime_events("beforeinput", move |event: InputEvent| {
            *before.borrow_mut() = BeforeInput {
                input_type: event.input_type(),
                data: event.data(),
                cancelable: event.cancelable(),
            };
        });
        let handler = Rc::new(RefCell::new(handler));
        self.on_text_input = self.add_ime_events("input", move |event: InputEvent| {
            let selection_start = element.selection_start().unwrap_or(0);
            let selection_end = element.selection_end().unwrap_or(selection_start);
            let selection_direction = match element.selection_direction().as_deref() {
                Some("backward") => WebSelectionDirection::Backward,
                Some("forward") => WebSelectionDirection::Forward,
                _ => WebSelectionDirection::None,
            };
            let before = std::mem::take(&mut *pending.borrow_mut());
            let input_type = event.input_type();
            handler.borrow_mut()(WebTextInputEvent {
                value: element.value(),
                selection_start,
                selection_end,
                selection_direction,
                input_type: if input_type.is_empty() { before.input_type } else { input_type },
                data: event.data().or(before.data),
                is_composing: event.is_composing(),
                before_input_cancelable: before.cancelable,
            });
        });
    }

    fn add_ime_events<E, F>(
        &self,
        event_name: &'static str,
        handler: F,
    ) -> Vec<EventListenerHandle<dyn FnMut(E)>>
    where
        E: 'static + AsRef<web_sys::Event> + wasm_bindgen::convert::FromWasmAbi,
        F: 'static + FnMut(E),
    {
        let handler = Rc::new(RefCell::new(handler));
        self.ime_element
            .event_targets()
            .into_iter()
            .map(|target| {
                let handler = Rc::clone(&handler);
                EventListenerHandle::new(
                    target,
                    event_name,
                    Closure::new(move |event| handler.borrow_mut()(event)),
                )
            })
            .collect()
    }

    pub fn set_ime_allowed(&self, allowed: bool) {
        if self.ime_allowed.replace(allowed) == allowed {
            return;
        }
        if allowed {
            self.ime_element.focus();
        } else {
            let _ = self.common.raw.focus();
        }
    }

    pub fn set_ime_cursor_area(&self, position: PhysicalPosition<f64>) {
        let scale = super::scale_factor(&self.common.window);
        let canvas_position = self.position();
        let position = position.to_logical::<f64>(scale);
        let style = self.ime_element.style();
        style.set_property("left", &format!("{}px", canvas_position.x + position.x)).ok();
        style.set_property("top", &format!("{}px", canvas_position.y + position.y)).ok();
    }

    pub fn set_ime_purpose(&self, purpose: crate::window::ImePurpose) {
        let input_mode = match purpose {
            crate::window::ImePurpose::Terminal => "text",
            _ => "text",
        };
        self.ime_element.set_attribute("inputmode", input_mode);
    }

    pub fn set_ime_text_state(
        &self,
        value: &str,
        selection_start: u32,
        selection_end: u32,
        selection_direction: WebSelectionDirection,
    ) {
        if self.ime_element.value() != value {
            self.ime_element.set_value(value);
        }
        let direction = match selection_direction {
            WebSelectionDirection::Backward => "backward",
            WebSelectionDirection::Forward => "forward",
            WebSelectionDirection::None => "none",
        };
        let text_len = value.encode_utf16().count().min(u32::MAX as usize) as u32;
        let start = selection_start.min(text_len);
        let end = selection_end.min(text_len);
        self.ime_element.set_selection_range(start, end, direction);
    }

    pub fn set_web_ime_configuration(&self, configuration: &crate::window::WebImeConfiguration) {
        self.ime_element.set_password_active(configuration.secure);
        for (name, value) in [
            ("name", configuration.name.as_str()),
            ("inputmode", configuration.input_mode.as_str()),
            ("enterkeyhint", configuration.enter_key_hint.as_str()),
            ("autocomplete", configuration.autocomplete.as_str()),
            ("autocapitalize", configuration.autocapitalize.as_str()),
            ("autocorrect", if configuration.autocorrect { "on" } else { "off" }),
            ("spellcheck", if configuration.spellcheck { "true" } else { "false" }),
            ("aria-label", configuration.aria_label.as_str()),
            ("aria-required", if configuration.required { "true" } else { "false" }),
            ("aria-invalid", if configuration.invalid { "true" } else { "false" }),
            ("aria-description", configuration.aria_description.as_str()),
        ] {
            if value.is_empty() {
                self.ime_element.remove_attribute(name);
            } else {
                self.ime_element.set_attribute(name, value);
            }
        }
    }

    pub fn on_focus<F>(&mut self, mut handler: F)
    where
        F: 'static + FnMut(),
    {
        self.on_focus = Some(self.common.add_event("focus", move |_: FocusEvent| {
            handler();
        }));
    }

    pub fn on_keyboard_release<F>(&mut self, handler: F)
    where
        F: 'static + FnMut(PhysicalKey, Key, Option<SmolStr>, KeyLocation, bool, ModifiersState),
    {
        let handler = Rc::new(std::cell::RefCell::new(handler));
        let make_listener =
            |target: EventTarget,
             handler: Rc<std::cell::RefCell<F>>,
             prevent_default: Rc<Cell<bool>>,
             browser_defaults: Rc<Cell<BrowserDefaults>>| {
                EventListenerHandle::new(
                    target,
                    "keyup",
                    Closure::new(move |event: KeyboardEvent| {
                        if prevent_default.get()
                            && !browser_defaults.get().contains(BrowserDefaults::KEYBOARD)
                            && !event.is_composing()
                            && !is_clipboard_shortcut(&event)
                        {
                            event.prevent_default();
                        }
                        let key = event::key(&event);
                        let modifiers = event::keyboard_modifiers(&event);
                        handler.borrow_mut()(
                            event::key_code(&event),
                            key,
                            event::key_text(&event),
                            event::key_location(&event),
                            event.repeat(),
                            modifiers,
                        );
                    }),
                )
            };
        self.on_keyboard_release = Some(make_listener(
            self.common.raw().clone().into(),
            Rc::clone(&handler),
            Rc::clone(&self.prevent_default),
            Rc::clone(&self.browser_defaults),
        ));
        self.on_ime_keyboard_release = self
            .ime_element
            .event_targets()
            .into_iter()
            .map(|target| {
                make_listener(
                    target,
                    Rc::clone(&handler),
                    Rc::clone(&self.prevent_default),
                    Rc::clone(&self.browser_defaults),
                )
            })
            .collect();
    }

    pub fn on_keyboard_press<F>(&mut self, handler: F)
    where
        F: 'static + FnMut(PhysicalKey, Key, Option<SmolStr>, KeyLocation, bool, ModifiersState),
    {
        let handler = Rc::new(std::cell::RefCell::new(handler));
        let make_listener =
            |target: EventTarget,
             handler: Rc<std::cell::RefCell<F>>,
             prevent_default: Rc<Cell<bool>>,
             browser_defaults: Rc<Cell<BrowserDefaults>>| {
                EventListenerHandle::new(
                    target,
                    "keydown",
                    Closure::new(move |event: KeyboardEvent| {
                        if prevent_default.get()
                            && !browser_defaults.get().contains(BrowserDefaults::KEYBOARD)
                            && !event.is_composing()
                            && !is_clipboard_shortcut(&event)
                        {
                            event.prevent_default();
                        }
                        let key = event::key(&event);
                        let modifiers = event::keyboard_modifiers(&event);
                        handler.borrow_mut()(
                            event::key_code(&event),
                            key,
                            event::key_text(&event),
                            event::key_location(&event),
                            event.repeat(),
                            modifiers,
                        );
                    }),
                )
            };
        self.on_keyboard_press = Some(make_listener(
            self.common.raw().clone().into(),
            Rc::clone(&handler),
            Rc::clone(&self.prevent_default),
            Rc::clone(&self.browser_defaults),
        ));
        self.on_ime_keyboard_press = self
            .ime_element
            .event_targets()
            .into_iter()
            .map(|target| {
                make_listener(
                    target,
                    Rc::clone(&handler),
                    Rc::clone(&self.prevent_default),
                    Rc::clone(&self.browser_defaults),
                )
            })
            .collect();
    }

    pub fn on_cursor_leave<F>(&mut self, handler: F)
    where
        F: 'static + FnMut(ModifiersState, Option<i32>),
    {
        self.pointer_handler.on_cursor_leave(&self.common, handler)
    }

    pub fn on_cursor_enter<F>(&mut self, handler: F)
    where
        F: 'static + FnMut(ModifiersState, Option<i32>),
    {
        self.pointer_handler.on_cursor_enter(&self.common, handler)
    }

    pub fn on_mouse_release<M, T>(&mut self, mouse_handler: M, touch_handler: T)
    where
        M: 'static + FnMut(ModifiersState, i32, PhysicalPosition<f64>, MouseButton),
        T: 'static + FnMut(ModifiersState, i32, PhysicalPosition<f64>, Force),
    {
        self.pointer_handler.on_mouse_release(&self.common, mouse_handler, touch_handler)
    }

    pub fn on_mouse_press<M, T>(&mut self, mouse_handler: M, touch_handler: T)
    where
        M: 'static + FnMut(ModifiersState, i32, PhysicalPosition<f64>, MouseButton),
        T: 'static + FnMut(ModifiersState, i32, PhysicalPosition<f64>, Force),
    {
        self.pointer_handler.on_mouse_press(
            &self.common,
            mouse_handler,
            touch_handler,
            Rc::clone(&self.prevent_default),
            Rc::clone(&self.browser_defaults),
            Rc::clone(&self.ime_allowed),
            self.ime_element.clone(),
        )
    }

    pub fn on_cursor_move<M, T, B>(&mut self, mouse_handler: M, touch_handler: T, button_handler: B)
    where
        M: 'static + FnMut(ModifiersState, i32, &mut dyn Iterator<Item = PhysicalPosition<f64>>),
        T: 'static
            + FnMut(ModifiersState, i32, &mut dyn Iterator<Item = (PhysicalPosition<f64>, Force)>),
        B: 'static + FnMut(ModifiersState, i32, PhysicalPosition<f64>, ButtonsState, MouseButton),
    {
        self.pointer_handler.on_cursor_move(
            &self.common,
            mouse_handler,
            touch_handler,
            button_handler,
            Rc::clone(&self.prevent_default),
            Rc::clone(&self.browser_defaults),
            Rc::clone(&self.ime_allowed),
            self.ime_element.clone(),
        )
    }

    pub fn on_touch_cancel<F>(&mut self, handler: F)
    where
        F: 'static + FnMut(i32, PhysicalPosition<f64>, Force),
    {
        self.pointer_handler.on_touch_cancel(&self.common, handler)
    }

    pub fn on_mouse_wheel<F>(&mut self, mut handler: F)
    where
        F: 'static + FnMut(i32, MouseScrollDelta, ModifiersState),
    {
        let window = self.common.window.clone();
        let prevent_default = Rc::clone(&self.prevent_default);
        let browser_defaults = Rc::clone(&self.browser_defaults);
        self.on_mouse_wheel = Some(self.common.add_event("wheel", move |event: WheelEvent| {
            if prevent_default.get() && !browser_defaults.get().contains(BrowserDefaults::WHEEL) {
                event.prevent_default();
            }

            if let Some(delta) = event::mouse_scroll_delta(&window, &event) {
                let modifiers = event::mouse_modifiers(&event);
                handler(0, delta, modifiers);
            }
        }));
    }

    pub fn on_dark_mode<F>(&mut self, mut handler: F)
    where
        F: 'static + FnMut(bool),
    {
        self.on_dark_mode = Some(MediaQueryListHandle::new(
            &self.common.window,
            "(prefers-color-scheme: dark)",
            move |mql| handler(mql.matches()),
        ));
    }

    pub(crate) fn on_resize_scale<S, R>(&mut self, scale_handler: S, size_handler: R)
    where
        S: 'static + Fn(PhysicalSize<u32>, f64),
        R: 'static + Fn(PhysicalSize<u32>),
    {
        self.on_resize_scale = Some(ResizeScaleHandle::new(
            self.window().clone(),
            self.document().clone(),
            self.raw().clone(),
            self.style().clone(),
            scale_handler,
            size_handler,
        ));
    }

    pub(crate) fn on_intersection<F>(&mut self, handler: F)
    where
        F: 'static + FnMut(bool),
    {
        self.on_intersect = Some(IntersectionObserverHandle::new(self.raw(), handler));
    }

    pub(crate) fn on_animation_frame<F>(&mut self, f: F)
    where
        F: 'static + FnMut(),
    {
        self.animation_frame_handler.on_animation_frame(f)
    }

    pub(crate) fn on_context_menu<F>(&mut self, mut handler: F)
    where
        F: 'static + FnMut(PhysicalPosition<f64>, ModifiersState),
    {
        let window = self.common.window.clone();
        let prevent_default = Rc::clone(&self.prevent_default);
        let browser_defaults = Rc::clone(&self.browser_defaults);
        self.on_context_menu =
            Some(self.common.add_event("contextmenu", move |event: PointerEvent| {
                if prevent_default.get()
                    && !browser_defaults.get().contains(BrowserDefaults::CONTEXT_MENU)
                {
                    event.prevent_default();
                }
                handler(
                    event::mouse_position(&event).to_physical(super::scale_factor(&window)),
                    event::mouse_modifiers(&event),
                );
            }));
    }

    pub(crate) fn on_clipboard<F>(&mut self, handler: F)
    where
        F: 'static + FnMut(WebClipboardEvent),
    {
        let handler = Rc::new(std::cell::RefCell::new(handler));
        self.on_copy = Some(self.add_clipboard_listener(
            "copy",
            WebClipboardAction::Copy,
            Rc::clone(&handler),
        ));
        self.on_cut =
            Some(self.add_clipboard_listener("cut", WebClipboardAction::Cut, Rc::clone(&handler)));
        self.on_paste =
            Some(self.add_clipboard_listener("paste", WebClipboardAction::Paste, handler));
    }

    fn add_clipboard_listener<F>(
        &self,
        event_name: &'static str,
        action: WebClipboardAction,
        handler: Rc<std::cell::RefCell<F>>,
    ) -> EventListenerHandle<dyn FnMut(ClipboardEvent)>
    where
        F: 'static + FnMut(WebClipboardEvent),
    {
        let prevent_default = Rc::clone(&self.prevent_default);
        let browser_defaults = Rc::clone(&self.browser_defaults);
        let canvas: EventTarget = self.common.raw().clone().into();
        let ime = self.ime_element.clone();
        EventListenerHandle::new(
            self.common.document.clone(),
            event_name,
            Closure::new(move |event: ClipboardEvent| {
                let Some(target) = event.target() else {
                    return;
                };
                if target != canvas && !ime.contains_target(&target) {
                    return;
                }
                let clipboard = event.clipboard_data();
                let text = (action == WebClipboardAction::Paste)
                    .then(|| clipboard.as_ref()?.get_data("text/plain").ok())
                    .flatten();
                let request = WebClipboardEvent::new(action, text);
                handler.borrow_mut()(request.clone());

                let response = request.take_response();
                if let (Some(clipboard), Some(text)) = (clipboard, response.as_deref()) {
                    let _ = clipboard.set_data("text/plain", text);
                }

                if response.is_some()
                    || (prevent_default.get()
                        && !browser_defaults.get().contains(BrowserDefaults::CLIPBOARD))
                {
                    event.prevent_default();
                }
            }),
        )
    }

    pub fn request_fullscreen(&self) {
        fullscreen::request_fullscreen(self.document(), self.raw());
    }

    pub fn exit_fullscreen(&self) {
        fullscreen::exit_fullscreen(self.document(), self.raw());
    }

    pub fn is_fullscreen(&self) -> bool {
        fullscreen::is_fullscreen(self.document(), self.raw())
    }

    pub fn request_animation_frame(&self) {
        self.animation_frame_handler.request();
    }

    pub(crate) fn handle_scale_change(
        &self,
        runner: &super::super::event_loop::runner::Shared,
        event_handler: impl FnOnce(crate::event::Event<()>),
        current_size: PhysicalSize<u32>,
        scale: f64,
    ) {
        // First, we send the `ScaleFactorChanged` event:
        self.set_current_size(current_size);
        let new_size = {
            let new_size = Arc::new(Mutex::new(current_size));
            event_handler(crate::event::Event::WindowEvent {
                window_id: RootWindowId(self.id),
                event: crate::event::WindowEvent::ScaleFactorChanged {
                    scale_factor: scale,
                    inner_size_writer: InnerSizeWriter::new(Arc::downgrade(&new_size)),
                },
            });

            let new_size = *new_size.lock().unwrap();
            new_size
        };

        if current_size != new_size {
            // Then we resize the canvas to the new size, a new
            // `Resized` event will be sent by the `ResizeObserver`:
            let new_size = new_size.to_logical(scale);
            super::set_canvas_size(self.document(), self.raw(), self.style(), new_size);

            // Set the size might not trigger the event because the calculation is inaccurate.
            self.on_resize_scale
                .as_ref()
                .expect("expected Window to still be active")
                .notify_resize();
        } else if self.old_size() != new_size {
            // Then we at least send a resized event.
            self.set_old_size(new_size);
            runner.send_event(crate::event::Event::WindowEvent {
                window_id: RootWindowId(self.id),
                event: crate::event::WindowEvent::Resized(new_size),
            })
        }
    }

    pub fn remove_listeners(&mut self) {
        self.on_touch_start = None;
        self.on_focus = None;
        self.on_blur = None;
        self.on_keyboard_release = None;
        self.on_keyboard_press = None;
        self.on_ime_keyboard_release.clear();
        self.on_ime_keyboard_press.clear();
        self.on_mouse_wheel = None;
        self.on_dark_mode = None;
        self.pointer_handler.remove_listeners();
        self.on_resize_scale = None;
        self.on_intersect = None;
        self.animation_frame_handler.cancel();
        self.on_touch_end = None;
        self.on_context_menu = None;
        self.on_copy = None;
        self.on_cut = None;
        self.on_paste = None;
        self.on_ime_focus.clear();
        self.on_ime_blur.clear();
        self.on_composition_start.clear();
        self.on_composition_update.clear();
        self.on_composition_end.clear();
        self.on_before_input.clear();
        self.on_text_input.clear();
        self.ime_element.remove();
    }
}

impl Common {
    pub fn add_event<E, F>(
        &self,
        event_name: &'static str,
        handler: F,
    ) -> EventListenerHandle<dyn FnMut(E)>
    where
        E: 'static + AsRef<web_sys::Event> + wasm_bindgen::convert::FromWasmAbi,
        F: 'static + FnMut(E),
    {
        EventListenerHandle::new(self.raw.deref().clone(), event_name, Closure::new(handler))
    }

    pub fn raw(&self) -> &HtmlCanvasElement {
        &self.raw
    }
}

fn is_clipboard_shortcut(event: &KeyboardEvent) -> bool {
    is_clipboard_accelerator(
        &event.key(),
        event.ctrl_key(),
        event.meta_key(),
        event.shift_key(),
        event.alt_key(),
    )
}

fn is_clipboard_accelerator(key: &str, ctrl: bool, meta: bool, shift: bool, alt: bool) -> bool {
    if alt {
        return false;
    }
    let primary = ctrl || meta;
    let key = key.to_ascii_lowercase();
    (primary && matches!(key.as_str(), "c" | "x" | "v"))
        || (ctrl && key == "insert")
        || (shift && matches!(key.as_str(), "insert" | "delete"))
}

#[cfg(test)]
mod tests {
    use super::is_clipboard_accelerator;

    #[test]
    fn recognizes_browser_clipboard_accelerators() {
        assert!(is_clipboard_accelerator("c", false, true, false, false));
        assert!(is_clipboard_accelerator("V", true, false, false, false));
        assert!(is_clipboard_accelerator("Insert", true, false, false, false));
        assert!(is_clipboard_accelerator("Delete", false, false, true, false));
    }

    #[test]
    fn does_not_release_unrelated_or_alt_graph_chords() {
        assert!(!is_clipboard_accelerator("a", false, true, false, false));
        assert!(!is_clipboard_accelerator("v", true, false, false, true));
        assert!(!is_clipboard_accelerator("v", false, false, false, false));
    }
}

impl Style {
    fn new(window: &web_sys::Window, canvas: &HtmlCanvasElement) -> Self {
        #[allow(clippy::disallowed_methods)]
        let read = window
            .get_computed_style(canvas)
            .expect("Failed to obtain computed style")
            // this can't fail: we aren't using a pseudo-element
            .expect("Invalid pseudo-element");

        #[allow(clippy::disallowed_methods)]
        let write = canvas.style();

        Self { read, write }
    }

    pub(crate) fn get(&self, property: &str) -> String {
        self.read.get_property_value(property).expect("Invalid property")
    }

    pub(crate) fn remove(&self, property: &str) {
        self.write.remove_property(property).expect("Property is read only");
    }

    pub(crate) fn set(&self, property: &str, value: &str) {
        self.write.set_property(property, value).expect("Property is read only");
    }
}
