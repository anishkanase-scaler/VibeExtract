//! macOS Accessibility API — picker + subtree walk.
//!
//! Same FFI surface that's inlined into the native-extract binary, lifted into
//! a library and made reusable. Public API:
//!
//! - [`check_permission`] — `AXIsProcessTrusted`, with prompt
//! - [`current_cursor`] — `CGEventGetLocation`
//! - [`pick`] — hit-test at a screen point, walk to enclosing window, return [`crate::capture::PickedElement`]
//! - [`walk_subtree`] — recursive AX walk that produces a [`Node`] tree
//! - [`Node`] — what the native extractor consumes

use crate::capture::{PickedElement, ScreenPoint, ScreenRect};
use anyhow::{anyhow, bail, Result};
use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFGetTypeID, CFRelease, CFTypeRef, TCFType, ToVoid};
use core_foundation::boolean::{kCFBooleanTrue, CFBoolean};
use core_foundation::dictionary::CFDictionary;
use core_foundation::string::{CFString, CFStringRef};
use core_graphics::display::CGPoint;
use serde::{Deserialize, Serialize};
use std::ffi::c_void;

// --- FFI ---------------------------------------------------------------------

#[allow(non_camel_case_types)]
pub type AXUIElementRef = *const c_void;
#[allow(non_camel_case_types)]
type AXValueRef = *const c_void;
#[allow(non_camel_case_types)]
type AXError = i32;

const K_AX_ERROR_SUCCESS: AXError = 0;
const K_AX_VALUE_TYPE_CG_POINT: u32 = 1;
const K_AX_VALUE_TYPE_CG_SIZE: u32 = 2;

#[link(name = "ApplicationServices", kind = "framework")]
extern "C" {
    fn AXIsProcessTrusted() -> bool;
    fn AXIsProcessTrustedWithOptions(options: *const c_void) -> bool;
    fn AXUIElementCreateSystemWide() -> AXUIElementRef;
    fn AXUIElementCreateApplication(pid: i32) -> AXUIElementRef;
    fn AXUIElementCopyElementAtPosition(
        application: AXUIElementRef,
        x: f32,
        y: f32,
        element: *mut AXUIElementRef,
    ) -> AXError;
    fn AXUIElementCopyAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: *mut CFTypeRef,
    ) -> AXError;
    fn AXUIElementSetAttributeValue(
        element: AXUIElementRef,
        attribute: CFStringRef,
        value: CFTypeRef,
    ) -> AXError;
    fn AXUIElementGetPid(element: AXUIElementRef, pid: *mut i32) -> AXError;
    fn AXValueGetType(value: AXValueRef) -> u32;
    fn AXValueGetValue(value: AXValueRef, the_type: u32, value_ptr: *mut c_void) -> bool;
}

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGEventCreate(source: *const c_void) -> *const c_void;
    fn CGEventGetLocation(event: *const c_void) -> CGPoint;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFRetain(cf: *const c_void) -> *const c_void;
    fn CFArrayGetTypeID() -> usize;
}

// --- Safe wrappers -----------------------------------------------------------

pub struct AxElement(pub AXUIElementRef);
impl Drop for AxElement {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { CFRelease(self.0 as *const _) };
        }
    }
}

impl AxElement {
    pub fn pid(&self) -> Option<i32> {
        let mut p: i32 = 0;
        let err = unsafe { AXUIElementGetPid(self.0, &mut p) };
        if err == K_AX_ERROR_SUCCESS {
            Some(p)
        } else {
            None
        }
    }

    pub fn str_attr(&self, key: &str) -> Option<String> {
        let key_cf = CFString::new(key);
        let mut value: CFTypeRef = std::ptr::null();
        let err =
            unsafe { AXUIElementCopyAttributeValue(self.0, key_cf.as_concrete_TypeRef(), &mut value) };
        if err != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        let type_id = unsafe { CFGetTypeID(value) };
        if type_id == CFString::type_id() {
            let s = unsafe { CFString::wrap_under_create_rule(value as CFStringRef) };
            Some(s.to_string())
        } else {
            unsafe { CFRelease(value) };
            None
        }
    }

    pub fn point_attr(&self, key: &str) -> Option<ScreenPoint> {
        let key_cf = CFString::new(key);
        let mut value: CFTypeRef = std::ptr::null();
        let err =
            unsafe { AXUIElementCopyAttributeValue(self.0, key_cf.as_concrete_TypeRef(), &mut value) };
        if err != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        let ty = unsafe { AXValueGetType(value as AXValueRef) };
        if ty != K_AX_VALUE_TYPE_CG_POINT {
            unsafe { CFRelease(value) };
            return None;
        }
        let mut pt = CGPoint { x: 0.0, y: 0.0 };
        let ok = unsafe {
            AXValueGetValue(
                value as AXValueRef,
                K_AX_VALUE_TYPE_CG_POINT,
                &mut pt as *mut CGPoint as *mut c_void,
            )
        };
        unsafe { CFRelease(value) };
        if ok {
            Some(ScreenPoint { x: pt.x, y: pt.y })
        } else {
            None
        }
    }

    pub fn size_attr(&self, key: &str) -> Option<(f64, f64)> {
        #[repr(C)]
        struct CGSize {
            width: f64,
            height: f64,
        }
        let key_cf = CFString::new(key);
        let mut value: CFTypeRef = std::ptr::null();
        let err =
            unsafe { AXUIElementCopyAttributeValue(self.0, key_cf.as_concrete_TypeRef(), &mut value) };
        if err != K_AX_ERROR_SUCCESS || value.is_null() {
            return None;
        }
        let ty = unsafe { AXValueGetType(value as AXValueRef) };
        if ty != K_AX_VALUE_TYPE_CG_SIZE {
            unsafe { CFRelease(value) };
            return None;
        }
        let mut sz = CGSize { width: 0.0, height: 0.0 };
        let ok = unsafe {
            AXValueGetValue(
                value as AXValueRef,
                K_AX_VALUE_TYPE_CG_SIZE,
                &mut sz as *mut CGSize as *mut c_void,
            )
        };
        unsafe { CFRelease(value) };
        if ok {
            Some((sz.width, sz.height))
        } else {
            None
        }
    }

    pub fn rect(&self) -> Option<ScreenRect> {
        let pt = self.point_attr("AXPosition")?;
        let (w, h) = self.size_attr("AXSize")?;
        Some(ScreenRect {
            x: pt.x,
            y: pt.y,
            w,
            h,
        })
    }

    pub fn array_attr(&self, key: &str) -> Vec<AxElement> {
        let key_cf = CFString::new(key);
        let mut value: CFTypeRef = std::ptr::null();
        let err =
            unsafe { AXUIElementCopyAttributeValue(self.0, key_cf.as_concrete_TypeRef(), &mut value) };
        if err != K_AX_ERROR_SUCCESS || value.is_null() {
            return Vec::new();
        }
        let type_id = unsafe { CFGetTypeID(value) };
        if type_id != unsafe { CFArrayGetTypeID() } {
            unsafe { CFRelease(value) };
            return Vec::new();
        }
        let array = unsafe { CFArray::<*const c_void>::wrap_under_create_rule(value as CFArrayRef) };
        let len = array.len();
        let mut out = Vec::with_capacity(len as usize);
        for i in 0..len {
            let item = array.get(i).map(|r| *r).unwrap_or(std::ptr::null());
            if item.is_null() {
                continue;
            }
            unsafe { CFRetain(item as *const _) };
            out.push(AxElement(item as AXUIElementRef));
        }
        out
    }

    /// Number of immediate AX children (AXChildren, falling back to AXContents
    /// the way `deepen_at` does). A cheap "is this subtree rich?" signal:
    /// Electron's window-filling shallow containers report ~0 (web content isn't
    /// in AX), whereas a native window-sized group (e.g. Calendar's "Year
    /// Calendar Area") reports many. Used by `is_shallow_pick` so a legitimate
    /// native container isn't mistaken for an empty Electron placeholder.
    pub fn child_count(&self) -> usize {
        let n = self.array_attr("AXChildren").len();
        if n > 0 { n } else { self.array_attr("AXContents").len() }
    }

    /// Return this element's immediate parent via AXParent, or None at the
    /// top of the AX tree (e.g. on AXApplication). Caller owns the returned
    /// element.
    pub fn parent(&self) -> Option<AxElement> {
        let key_cf = CFString::new("AXParent");
        let mut parent: CFTypeRef = std::ptr::null();
        let err = unsafe {
            AXUIElementCopyAttributeValue(self.0, key_cf.as_concrete_TypeRef(), &mut parent)
        };
        if err != K_AX_ERROR_SUCCESS || parent.is_null() {
            return None;
        }
        Some(AxElement(parent as AXUIElementRef))
    }

    /// Copy an attribute that is itself an AX element (e.g. `AXMainWindow`,
    /// `AXFocusedWindow`). Caller owns the returned element (released on drop).
    pub fn element_attr(&self, key: &str) -> Option<AxElement> {
        let key_cf = CFString::new(key);
        let mut out: CFTypeRef = std::ptr::null();
        let err =
            unsafe { AXUIElementCopyAttributeValue(self.0, key_cf.as_concrete_TypeRef(), &mut out) };
        if err != K_AX_ERROR_SUCCESS || out.is_null() {
            return None;
        }
        Some(AxElement(out as AXUIElementRef))
    }

    pub fn enclosing_window(&self) -> Option<AxElement> {
        if let Some(role) = self.str_attr("AXRole") {
            if role == "AXWindow" {
                // Re-wrap by retaining self's ref so caller gets its own owned element.
                unsafe { CFRetain(self.0 as *const _) };
                return Some(AxElement(self.0));
            }
        }
        let mut current = self.0;
        for _ in 0..50 {
            let key_cf = CFString::new("AXParent");
            let mut parent: CFTypeRef = std::ptr::null();
            let err =
                unsafe { AXUIElementCopyAttributeValue(current, key_cf.as_concrete_TypeRef(), &mut parent) };
            if err != K_AX_ERROR_SUCCESS || parent.is_null() {
                return None;
            }
            let wrapped = AxElement(parent as AXUIElementRef);
            let role = wrapped.str_attr("AXRole").unwrap_or_default();
            if role == "AXWindow" {
                return Some(wrapped);
            }
            current = wrapped.0;
            std::mem::forget(wrapped);
        }
        None
    }
}

// --- Permission / cursor / hit-test ------------------------------------------

pub fn check_permission(prompt: bool) -> bool {
    if unsafe { AXIsProcessTrusted() } {
        return true;
    }
    if !prompt {
        return false;
    }
    let key = CFString::from_static_string("AXTrustedCheckOptionPrompt");
    let value = unsafe { CFBoolean::wrap_under_get_rule(kCFBooleanTrue) };
    let dict: CFDictionary<CFString, CFBoolean> = CFDictionary::from_CFType_pairs(&[(key, value)]);
    unsafe { AXIsProcessTrustedWithOptions(dict.to_void()) }
}

pub fn open_accessibility_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_Accessibility")
        .status();
}

pub fn current_cursor() -> ScreenPoint {
    unsafe {
        let event = CGEventCreate(std::ptr::null());
        if event.is_null() {
            return ScreenPoint { x: -1.0, y: -1.0 };
        }
        let pt = CGEventGetLocation(event);
        CFRelease(event);
        ScreenPoint { x: pt.x, y: pt.y }
    }
}

/// Force an Electron / Chromium app to expose its accessibility tree.
///
/// By default Electron apps build an empty/collapsed AX tree until another
/// process flips one of two attributes on their AXApplication element. We try
/// both attribute names (older Electron uses `AXEnhancedUserInterface`, newer
/// ones look for `AXManualAccessibility`). The call is idempotent and cheap —
/// safe to invoke for native AppKit apps too (they'll just ignore it).
///
/// Call this once per pid the first time you encounter that pid in pick mode;
/// the side-effect persists for the lifetime of the target process.
pub fn wake_app_ax(pid: i32) {
    if pid <= 0 {
        return;
    }
    let app: AXUIElementRef = unsafe { AXUIElementCreateApplication(pid) };
    if app.is_null() {
        return;
    }
    let true_ref = unsafe { kCFBooleanTrue as CFTypeRef };
    for attr_name in &["AXEnhancedUserInterface", "AXManualAccessibility"] {
        let key = CFString::new(attr_name);
        let _ = unsafe { AXUIElementSetAttributeValue(app, key.as_concrete_TypeRef(), true_ref) };
    }
    unsafe { CFRelease(app as *const _) };
}

pub fn element_at(point: ScreenPoint) -> Option<AxElement> {
    let system = unsafe { AXUIElementCreateSystemWide() };
    let mut out: AXUIElementRef = std::ptr::null();
    let err =
        unsafe { AXUIElementCopyElementAtPosition(system, point.x as f32, point.y as f32, &mut out) };
    unsafe { CFRelease(system as *const _) };
    if err != K_AX_ERROR_SUCCESS || out.is_null() {
        None
    } else {
        Some(AxElement(out))
    }
}

// --- CGWindowList-based "skip our own process" hit-test ----------------------

#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGWindowListCopyWindowInfo(
        option: u32,
        relative_to_window: u32,
    ) -> *const c_void;
}

const K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY: u32 = 1 << 0;
const K_CG_NULL_WINDOW_ID: u32 = 0;

use core_foundation::dictionary::CFDictionaryRef;
use core_foundation::number::CFNumberRef;

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFDictionaryGetValue(theDict: CFDictionaryRef, key: *const c_void) -> *const c_void;
    fn CFNumberGetValue(number: CFNumberRef, the_type: i32, value_ptr: *mut c_void) -> bool;
}

// CFNumberType: kCFNumberCGFloatType=16, kCFNumberDoubleType=13, kCFNumberSInt32Type=3
const K_CF_NUMBER_SINT32: i32 = 3;
const K_CF_NUMBER_DOUBLE: i32 = 13;

fn cf_dict_get(dict: CFDictionaryRef, key: &str) -> *const c_void {
    let cf_key = CFString::new(key);
    unsafe { CFDictionaryGetValue(dict, cf_key.as_concrete_TypeRef() as *const c_void) }
}

fn read_i32(dict: CFDictionaryRef, key: &str) -> Option<i32> {
    let v = cf_dict_get(dict, key);
    if v.is_null() {
        return None;
    }
    let mut out: i32 = 0;
    let ok = unsafe { CFNumberGetValue(v as CFNumberRef, K_CF_NUMBER_SINT32, &mut out as *mut i32 as *mut c_void) };
    if ok { Some(out) } else { None }
}

fn read_f64(dict: CFDictionaryRef, key: &str) -> Option<f64> {
    let v = cf_dict_get(dict, key);
    if v.is_null() {
        return None;
    }
    let mut out: f64 = 0.0;
    let ok = unsafe { CFNumberGetValue(v as CFNumberRef, K_CF_NUMBER_DOUBLE, &mut out as *mut f64 as *mut c_void) };
    if ok { Some(out) } else { None }
}

/// Find the topmost on-screen window at `point` whose owner process is NOT
/// `skip_pid`. Returns the owning pid + window bounds. Used to bypass our own
/// overlay window when AX-hit-testing.
pub fn topmost_window_owner_at(point: ScreenPoint, skip_pid: i32) -> Option<(i32, ScreenRect)> {
    let info_ref: *const c_void = unsafe {
        CGWindowListCopyWindowInfo(K_CG_WINDOW_LIST_OPTION_ON_SCREEN_ONLY, K_CG_NULL_WINDOW_ID)
    };
    if info_ref.is_null() {
        return None;
    }
    let array: CFArray<*const c_void> = unsafe { CFArray::wrap_under_create_rule(info_ref as _) };
    for i in 0..array.len() {
        let item = array.get(i).map(|r| *r).unwrap_or(std::ptr::null());
        if item.is_null() {
            continue;
        }
        let dict = item as CFDictionaryRef;
        let Some(owner) = read_i32(dict, "kCGWindowOwnerPID") else { continue };
        if owner == skip_pid || owner <= 0 {
            continue;
        }
        // Read window bounds — keys: X, Y, Width, Height (CFNumber doubles).
        let bounds_ref = cf_dict_get(dict, "kCGWindowBounds");
        if bounds_ref.is_null() {
            continue;
        }
        let b = bounds_ref as CFDictionaryRef;
        let x = read_f64(b, "X").unwrap_or(0.0);
        let y = read_f64(b, "Y").unwrap_or(0.0);
        let w = read_f64(b, "Width").unwrap_or(0.0);
        let h = read_f64(b, "Height").unwrap_or(0.0);
        if w <= 1.0 || h <= 1.0 {
            continue;
        }
        let contains = point.x >= x && point.x <= x + w && point.y >= y && point.y <= y + h;
        if contains {
            // Skip the system-wide menubar/Dock processes only if their window
            // doesn't actually contain the cursor — but since we checked above,
            // we accept this as the answer.
            return Some((owner, ScreenRect { x, y, w, h }));
        }
    }
    None
}

/// Hit-test that explicitly skips windows owned by `skip_pid` (i.e. ours).
/// Returns the deepest AX element in the topmost other app at the point.
pub fn element_at_excluding(point: ScreenPoint, skip_pid: i32) -> Option<AxElement> {
    let (target_pid, _bounds) = topmost_window_owner_at(point, skip_pid)?;
    let app: AXUIElementRef = unsafe { AXUIElementCreateApplication(target_pid) };
    if app.is_null() {
        return None;
    }
    let mut out: AXUIElementRef = std::ptr::null();
    let err = unsafe { AXUIElementCopyElementAtPosition(app, point.x as f32, point.y as f32, &mut out) };
    unsafe { CFRelease(app as *const _) };
    if err != K_AX_ERROR_SUCCESS || out.is_null() {
        None
    } else {
        Some(AxElement(out))
    }
}

/// Get the pid of the system's currently-focused (frontmost) application
/// via `AXFocusedApplication` on the system-wide AX root. This bypasses
/// CGWindowList entirely and gives us the *real* main process pid that owns
/// the user's attention — for Electron apps that's the main process, not a
/// helper / renderer subprocess.
pub fn frontmost_app_pid() -> Option<i32> {
    let system = unsafe { AXUIElementCreateSystemWide() };
    if system.is_null() {
        return None;
    }
    let key = CFString::new("AXFocusedApplication");
    let mut app: CFTypeRef = std::ptr::null();
    let err = unsafe { AXUIElementCopyAttributeValue(system, key.as_concrete_TypeRef(), &mut app) };
    unsafe { CFRelease(system as *const _) };
    if err != K_AX_ERROR_SUCCESS || app.is_null() {
        return None;
    }
    let mut pid: i32 = -1;
    let perr = unsafe { AXUIElementGetPid(app as AXUIElementRef, &mut pid) };
    unsafe { CFRelease(app) };
    if perr != K_AX_ERROR_SUCCESS || pid <= 0 {
        return None;
    }
    Some(pid)
}

/// Hit-test inside a specific app's AX tree. Used when we already know which
/// app the user is interacting with (e.g. from `frontmost_app_pid`).
pub fn element_at_in_app(point: ScreenPoint, app_pid: i32) -> Option<AxElement> {
    let app: AXUIElementRef = unsafe { AXUIElementCreateApplication(app_pid) };
    if app.is_null() {
        return None;
    }
    let mut out: AXUIElementRef = std::ptr::null();
    let err = unsafe { AXUIElementCopyElementAtPosition(app, point.x as f32, point.y as f32, &mut out) };
    unsafe { CFRelease(app as *const _) };
    if err != K_AX_ERROR_SUCCESS || out.is_null() {
        None
    } else {
        Some(AxElement(out))
    }
}

/// Full pick using a known app pid: hit-test in that app, deepen, build a
/// `PickedElement`. Bypasses CGWindowList entirely so Electron's helper-pid
/// confusion is moot.
pub fn pick_in_app(point: ScreenPoint, app_pid: i32) -> Result<PickedElement> {
    if !check_permission(false) {
        bail!("AX permission denied");
    }
    // Wake the app (idempotent — no-op for non-Electron).
    wake_app_ax(app_pid);
    let initial = element_at_in_app(point, app_pid)
        .ok_or_else(|| anyhow!("no AX element at ({},{}) in app pid {}", point.x, point.y, app_pid))?;
    let el = deepen_at(initial, point);
    let role = el.str_attr("AXRole").unwrap_or_else(|| "AXUnknown".into());
    let subrole = el.str_attr("AXSubrole").filter(|s| !s.is_empty());
    let name = el
        .str_attr("AXTitle")
        .or_else(|| el.str_attr("AXDescription"))
        .or_else(|| el.str_attr("AXLabel"))
        .or_else(|| el.str_attr("AXValue"))
        .unwrap_or_default();
    let identifier = el.str_attr("AXIdentifier").filter(|s| !s.is_empty());
    let bounds = el
        .rect()
        .ok_or_else(|| anyhow!("picked element has no bounds"))?;
    let (window_title, window_bounds) = match el.enclosing_window() {
        Some(w) => (w.str_attr("AXTitle"), w.rect()),
        None => (None, None),
    };
    let app_path = pid_to_path(app_pid);
    let ax_shallow = is_shallow_pick(&role, &bounds, point, window_bounds.as_ref(), el.child_count());
    Ok(PickedElement {
        role,
        subrole,
        name,
        identifier,
        bounds,
        pid: app_pid,
        app_path,
        window_title,
        window_bounds,
        click: Some(point),
        ax_shallow,
        crop_path: None,
        ax_tree: None,
    })
}

/// After an initial hit-test, walk the element's children to find the deepest
/// child whose bounds contain `point`. Necessary because Electron / Chromium
/// apps return shallow `AXWebArea` containers from the system-wide hit-test;
/// real leaf elements (buttons, message bubbles, text) are nested children we
/// have to descend into manually.
///
/// A no-op for native AppKit apps (their initial hit-test already returns the
/// leaf — child walk finds nothing more specific).
pub fn deepen_at(start: AxElement, point: ScreenPoint) -> AxElement {
    let mut current = start;
    for _ in 0..40 {
        // Try several children attributes — Electron / Chromium AX trees and
        // tables/outlines hide content under AXContents / AXRows / AXSections
        // rather than AXChildren. Shared with `capture_node` so the picker
        // descends as deeply as the tree walk does.
        let (kids, _) = enumerate_children(&current);
        // Find the child whose bounds tightly contain the point. If multiple
        // children match (e.g. overlapping), pick the smallest-area one — that
        // generally corresponds to the most specific leaf.
        let mut best: Option<(AxElement, f64)> = None;
        for k in kids {
            let Some(rect) = k.rect() else { continue };
            let contains = point.x >= rect.x
                && point.x <= rect.x + rect.w
                && point.y >= rect.y
                && point.y <= rect.y + rect.h;
            if !contains {
                continue;
            }
            let area = rect.w * rect.h;
            if best.as_ref().map(|(_, a)| area < *a).unwrap_or(true) {
                best = Some((k, area));
            }
        }
        match best {
            Some((deeper, _)) => current = deeper,
            None => return current,
        }
    }
    current
}

/// Heuristic: did the AX hit-test fail to land on a real content element the
/// user could have meant? True when the resolved element can't be the click
/// target — its bounds don't contain the click, or it's a top-level container
/// (menu bar / application / whole window) that Electron returns when its web
/// content isn't exposed to AX (so `AXUIElementCopyElementAtPosition` yields the
/// `AXMenuBar` at `{0,0,W,30}` instead of the clicked sidebar/message). Such a
/// result must NOT be committed as the pick: the caller keeps the click point
/// and re-resolves at it via the CDP ladder.
/// A window-filling element is treated as shallow only when it has at most this
/// many AX children — the Electron signature (one big empty container because web
/// content isn't exposed to AX). A native window-sized group has far more, so it
/// stays a real, selectable element.
const SHALLOW_MAX_CHILDREN: usize = 2;

pub fn is_shallow_pick(
    role: &str,
    bounds: &ScreenRect,
    click: ScreenPoint,
    window_bounds: Option<&ScreenRect>,
    child_count: usize,
) -> bool {
    // The element doesn't even cover where the user clicked.
    if !bounds.contains(click) {
        return true;
    }
    // Top-level containers are never the intended target of a content click.
    if matches!(
        role,
        "AXMenuBar" | "AXMenuBarItem" | "AXApplication" | "AXWindow"
    ) {
        return true;
    }
    // The element is ~the whole window. This is shallow ONLY when it also has
    // almost no AX children — i.e. Electron returned a big empty container
    // because its web content isn't in the AX tree. A NATIVE app's window-sized
    // group (e.g. Calendar's "Year Calendar Area", which holds the 12 month
    // grids) is rich and IS a legitimate pick — don't shrink it to a click box.
    if let Some(win) = window_bounds {
        let win_area = win.w * win.h;
        let el_area = bounds.w * bounds.h;
        if win_area > 0.0 && el_area >= 0.9 * win_area && child_count <= SHALLOW_MAX_CHILDREN {
            return true;
        }
    }
    false
}

/// Hit-test at `point`, walk to enclosing window, capture metadata.
///
/// Excludes the calling process from the hit-test (so e.g. our own overlay
/// window doesn't return its own AXWebArea when the user hovers above another
/// app's window).
pub fn pick(point: ScreenPoint) -> Result<PickedElement> {
    if !check_permission(false) {
        bail!("AX permission denied — grant Accessibility access in System Settings then retry.");
    }
    let our_pid = std::process::id() as i32;
    let initial = element_at_excluding(point, our_pid)
        .or_else(|| element_at(point))
        .ok_or_else(|| anyhow!("no AX element at ({}, {})", point.x, point.y))?;
    let el = deepen_at(initial, point);
    let role = el.str_attr("AXRole").unwrap_or_else(|| "AXUnknown".into());
    let subrole = el.str_attr("AXSubrole").filter(|s| !s.is_empty());
    let name = el
        .str_attr("AXTitle")
        .or_else(|| el.str_attr("AXDescription"))
        .or_else(|| el.str_attr("AXLabel"))
        .or_else(|| el.str_attr("AXValue"))
        .unwrap_or_default();
    let identifier = el.str_attr("AXIdentifier").filter(|s| !s.is_empty());
    let bounds = el
        .rect()
        .ok_or_else(|| anyhow!("element has no bounds"))?;
    let pid = el.pid().unwrap_or(-1);

    let (window_title, window_bounds) = match el.enclosing_window() {
        Some(w) => (w.str_attr("AXTitle"), w.rect()),
        None => (None, None),
    };

    // Look up the process path via /proc-equivalent on macOS (libproc).
    let app_path = pid_to_path(pid);
    let ax_shallow = is_shallow_pick(&role, &bounds, point, window_bounds.as_ref(), el.child_count());

    Ok(PickedElement {
        role,
        subrole,
        name,
        identifier,
        bounds,
        pid,
        app_path,
        window_title,
        window_bounds,
        click: Some(point),
        ax_shallow,
        crop_path: None,
        ax_tree: None,
    })
}

extern "C" {
    fn proc_pidpath(pid: i32, buffer: *mut c_void, buffersize: u32) -> i32;
}

const PROC_PIDPATHINFO_MAXSIZE: usize = 4 * 1024;

pub fn pid_to_path(pid: i32) -> Option<String> {
    if pid <= 0 {
        return None;
    }
    let mut buf: Vec<u8> = vec![0; PROC_PIDPATHINFO_MAXSIZE];
    let n = unsafe { proc_pidpath(pid, buf.as_mut_ptr() as *mut c_void, PROC_PIDPATHINFO_MAXSIZE as u32) };
    if n <= 0 {
        return None;
    }
    buf.truncate(n as usize);
    String::from_utf8(buf).ok()
}

// --- Subtree walk ------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Node {
    pub role: String,
    pub subrole: Option<String>,
    pub name: String,
    pub identifier: Option<String>,
    pub value: Option<String>,
    pub role_description: Option<String>,
    pub bounds: Option<ScreenRect>,
    /// Filled in by the sampling pass.
    pub bg: Option<(u8, u8, u8)>,
    /// Which AX relation produced this node's children when it wasn't the
    /// obvious AXChildren/AXVisibleChildren — e.g. "AXRows", "AXContents",
    /// "AXSections", or an "empty:…" honesty marker for an AX-opaque web
    /// surface. `None` for the common case. Surfaces *where* structure came
    /// from (or why it's missing) instead of a mysteriously flat tree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_source: Option<String>,
    pub children: Vec<Node>,
}

pub fn walk_subtree(point: ScreenPoint, max_depth: u32) -> Result<Node> {
    let root = element_at(point).ok_or_else(|| anyhow!("no AX element at point"))?;
    Ok(capture_node(&root, 0, max_depth))
}

pub fn walk_node(el: &AxElement, max_depth: u32) -> Node {
    capture_node(el, 0, max_depth)
}

fn capture_node(el: &AxElement, depth: u32, max_depth: u32) -> Node {
    let role = el.str_attr("AXRole").unwrap_or_else(|| "AXUnknown".into());
    let subrole = el.str_attr("AXSubrole").filter(|s| !s.is_empty());
    let name = el
        .str_attr("AXTitle")
        .or_else(|| el.str_attr("AXDescription"))
        .or_else(|| el.str_attr("AXLabel"))
        .or_else(|| el.str_attr("AXValue"))
        .unwrap_or_default();
    let identifier = el.str_attr("AXIdentifier").filter(|s| !s.is_empty());
    let value = el
        .str_attr("AXValue")
        .filter(|s| !s.is_empty() && Some(s) != Some(&name));
    let role_description = el
        .str_attr("AXRoleDescription")
        .filter(|s| !s.is_empty());
    let bounds = el.rect();

    let mut children = Vec::new();
    let mut child_source: Option<String> = None;
    if depth < max_depth {
        let (kids, rel) = enumerate_children(el);
        child_source = rel.map(|r| r.to_string());
        for kid in &kids {
            children.push(capture_node(kid, depth + 1, max_depth));
        }
    }
    // Honesty marker: a web surface that exposed NO children through ANY
    // relation is Chromium/CEF content AX can't see — the real structure lives
    // in the DOM and needs CDP. Flag it so a flat tree reads as "opaque here"
    // rather than a mysterious empty leaf. (Restricted to AXWebArea; an empty
    // AXGroup is too common to be a reliable signal.)
    if children.is_empty() && depth < max_depth && role == "AXWebArea" {
        child_source = Some("empty:web-content-opaque-to-AX (needs CDP)".into());
    }

    Node {
        role,
        subrole,
        name,
        identifier,
        value,
        role_description,
        bounds,
        bg: None,
        child_source,
        children,
    }
}

/// Enumerate an element's child elements, trying the standard relations first
/// and then the less-common ones some apps hide structure behind. Returns the
/// children plus the relation name when it WASN'T the obvious AXChildren /
/// AXVisibleChildren (so `capture_node` can record where structure came from).
///
/// Stops at the first non-empty relation — rows/contents are frequently ALSO
/// listed under AXChildren, so merging would double-count. The fallbacks
/// (AXContents = scroll areas; AXRows = tables/outlines/lists; AXSections =
/// web/structured documents) are why a previously-flat extraction now descends
/// into tables and scroll containers instead of stopping at the container.
fn enumerate_children(el: &AxElement) -> (Vec<AxElement>, Option<&'static str>) {
    let visible = el.array_attr("AXVisibleChildren");
    if !visible.is_empty() {
        return (visible, None);
    }
    let children = el.array_attr("AXChildren");
    if !children.is_empty() {
        return (children, None);
    }
    for rel in ["AXContents", "AXRows", "AXSections"] {
        let kids = el.array_attr(rel);
        if !kids.is_empty() {
            return (kids, Some(rel));
        }
    }
    (Vec::new(), None)
}

pub fn count_nodes(n: &Node) -> usize {
    1 + n.children.iter().map(count_nodes).sum::<usize>()
}

/// Grow from `el` to its sibling BAND — the run of same-parent children that share
/// `current`'s horizontal row (vertical overlap with `current` AND similar height).
/// Returns the band's bounding rect + each member's subtree (depth `max_depth`, ordered
/// left-to-right) so the caller can build a synthetic group. Returns `None` when there's
/// no real band (<2 members), the band ≈ the element, or the band ≈ the parent (so it
/// never duplicates an existing ↑ step). This is what lets ↑ select "the whole toolbar
/// row" in FLAT trees — e.g. WPS, whose ribbon tabs are direct `AXWindow` children with
/// no row container, so a plain parent-walk would jump straight to the window.
pub fn sibling_band(el: &AxElement, current: &ScreenRect, max_depth: u32) -> Option<(ScreenRect, Vec<Node>)> {
    if current.w < 1.0 || current.h < 1.0 {
        return None;
    }
    let parent = el.parent()?;
    let parent_rect = parent.rect();
    let (cy0, cy1) = (current.y, current.y + current.h);
    // Same-row siblings: vertical overlap > 50% of the current height AND similar height.
    let mut kept: Vec<(f64, AxElement, ScreenRect)> = Vec::new();
    for k in parent.array_attr("AXChildren") {
        let Some(r) = k.rect() else { continue };
        if r.w < 1.0 || r.h < 1.0 {
            continue;
        }
        let overlap = (r.y + r.h).min(cy1) - r.y.max(cy0);
        let height_similar = (r.h - current.h).abs() <= current.h * 0.6;
        if overlap > current.h * 0.5 && height_similar {
            kept.push((r.x, k, r));
        }
    }
    if kept.len() < 2 {
        return None;
    }
    let minx = kept.iter().map(|(_, _, r)| r.x).fold(f64::INFINITY, f64::min);
    let miny = kept.iter().map(|(_, _, r)| r.y).fold(f64::INFINITY, f64::min);
    let maxx = kept.iter().map(|(_, _, r)| r.x + r.w).fold(f64::NEG_INFINITY, f64::max);
    let maxy = kept.iter().map(|(_, _, r)| r.y + r.h).fold(f64::NEG_INFINITY, f64::max);
    let band = ScreenRect { x: minx, y: miny, w: maxx - minx, h: maxy - miny };
    // Must be meaningfully wider than the single element …
    if band.w < current.w * 1.5 {
        return None;
    }
    // … and not essentially the whole parent (else this is just the parent step).
    if let Some(p) = parent_rect {
        let pa = p.w * p.h;
        if pa > 0.0 && band.w * band.h >= 0.9 * pa {
            return None;
        }
    }
    kept.sort_by(|a, b| a.0.partial_cmp(&b.0).unwrap_or(std::cmp::Ordering::Equal));
    let nodes: Vec<Node> = kept.iter().map(|(_, k, _)| walk_node(k, max_depth)).collect();
    Some((band, nodes))
}

// --- Root walks (app / window) -----------------------------------------------
//
// `walk_subtree`/`walk_node` above start from a hit-tested point or a live
// element. The MCP server instead wants to walk from an app or window ROOT
// (no cursor involved), so Claude can inventory a whole window's AX tree.

/// How to pick which window of an app to walk.
#[derive(Debug, Clone)]
pub enum WindowSelector {
    /// Index into the app's `AXWindows` array (front-to-back order).
    Index(usize),
    /// Exact `AXTitle` match.
    Title(String),
    /// The app's main/focused window (falls back to the first window).
    Main,
    /// Match the Core Graphics window id from [`crate::windows_list::list_windows`]
    /// by reconciling its bounds against the AX windows (best-effort).
    WindowId(u32),
}

/// Build an owned `AxElement` for an application's AX root by pid.
fn app_element(pid: i32) -> Result<AxElement> {
    if pid <= 0 {
        bail!("invalid pid {pid}");
    }
    let app: AXUIElementRef = unsafe { AXUIElementCreateApplication(pid) };
    if app.is_null() {
        bail!("AXUIElementCreateApplication({pid}) returned null");
    }
    Ok(AxElement(app))
}

/// Walk the entire AX tree of an application from its root, to `max_depth`.
/// Wakes the app's AX tree first (idempotent; needed for Electron).
pub fn walk_app(pid: i32, max_depth: u32) -> Result<Node> {
    if !check_permission(false) {
        bail!("AX permission denied — grant Accessibility access in System Settings then retry.");
    }
    wake_app_ax(pid);
    let app = app_element(pid)?;
    Ok(walk_node(&app, max_depth))
}

/// Walk a single window of an application, selected by [`WindowSelector`].
pub fn walk_window(pid: i32, sel: WindowSelector, max_depth: u32) -> Result<Node> {
    if !check_permission(false) {
        bail!("AX permission denied — grant Accessibility access in System Settings then retry.");
    }
    wake_app_ax(pid);
    let app = app_element(pid)?;

    // Main: prefer the app's main/focused window element directly — it doesn't
    // depend on the AXWindows array being populated/ordered as expected.
    if let WindowSelector::Main = sel {
        if let Some(w) = app
            .element_attr("AXMainWindow")
            .or_else(|| app.element_attr("AXFocusedWindow"))
        {
            return Ok(walk_node(&w, max_depth));
        }
    }

    let windows = app.array_attr("AXWindows");
    if windows.is_empty() {
        bail!("app pid {pid} exposes no AX windows (Electron may need waking, or none are open)");
    }

    let idx = match sel {
        WindowSelector::Index(i) => i,
        WindowSelector::Main => 0,
        WindowSelector::Title(t) => windows
            .iter()
            .position(|w| w.str_attr("AXTitle").as_deref() == Some(t.as_str()))
            .ok_or_else(|| anyhow!("no window titled {t:?} in pid {pid}"))?,
        WindowSelector::WindowId(id) => {
            // Reconcile the CG window id to an AX window via bounds matching.
            match crate::windows_list::list_windows()
                .into_iter()
                .find(|w| w.window_id == id)
            {
                Some(target) => windows
                    .iter()
                    .position(|w| w.rect().map(|r| rect_close(&r, &target.bounds)).unwrap_or(false))
                    .unwrap_or(0),
                None => 0,
            }
        }
    };

    let chosen = windows
        .into_iter()
        .nth(idx)
        .ok_or_else(|| anyhow!("window index {idx} out of range for pid {pid}"))?;
    Ok(walk_node(&chosen, max_depth))
}

/// Loose rect equality (within 4pt) — AX vs CGWindowList bounds drift slightly.
fn rect_close(a: &ScreenRect, b: &ScreenRect) -> bool {
    (a.x - b.x).abs() < 4.0
        && (a.y - b.y).abs() < 4.0
        && (a.w - b.w).abs() < 4.0
        && (a.h - b.h).abs() < 4.0
}

/// Walk the app's main window, screenshot it, sample a deduped color palette
/// from the AX node centers. Reuses [`crate::sampling`] end-to-end.
pub fn window_palette(pid: i32, max_depth: u32) -> Result<Vec<(u8, u8, u8)>> {
    use base64::Engine as _;
    let mut node = walk_window(pid, WindowSelector::Main, max_depth)?;
    let root_bounds = node
        .bounds
        .ok_or_else(|| anyhow!("main window of pid {pid} has no bounds"))?;
    let shot = crate::screenshot::capture_region_b64(root_bounds)?;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(&shot.png_b64)
        .map_err(|e| anyhow!("base64 decode: {e}"))?;
    let img = image::load_from_memory(&bytes)?.into_rgba8();
    crate::sampling::fill_node_colors(&mut node, &img, root_bounds);
    let mut palette = Vec::new();
    crate::sampling::collect_palette(&node, &mut palette);
    Ok(palette)
}

#[cfg(test)]
mod shallow_tests {
    use super::is_shallow_pick;
    use crate::capture::{ScreenPoint, ScreenRect};

    #[test]
    fn menu_bar_is_shallow_even_when_it_contains_the_click() {
        // The exact bug: a Slack sidebar click resolves to the menu bar strip.
        let menubar = ScreenRect { x: 0.0, y: 0.0, w: 1440.0, h: 30.0 };
        assert!(is_shallow_pick("AXMenuBar", &menubar, ScreenPoint { x: 700.0, y: 10.0 }, None, 5));
    }

    #[test]
    fn click_outside_bounds_is_shallow() {
        let menubar = ScreenRect { x: 0.0, y: 0.0, w: 1440.0, h: 30.0 };
        // sidebar click far below the menu bar — bounds don't contain it.
        assert!(is_shallow_pick("AXMenuBar", &menubar, ScreenPoint { x: 150.0, y: 400.0 }, None, 0));
    }

    #[test]
    fn real_button_containing_click_is_not_shallow() {
        let btn = ScreenRect { x: 100.0, y: 380.0, w: 220.0, h: 34.0 };
        let win = ScreenRect { x: 0.0, y: 25.0, w: 1440.0, h: 875.0 };
        assert!(!is_shallow_pick("AXButton", &btn, ScreenPoint { x: 150.0, y: 400.0 }, Some(&win), 0));
    }

    #[test]
    fn empty_window_sized_element_is_shallow() {
        // Electron: AX returns one window-filling container with ~no children.
        let win = ScreenRect { x: 0.0, y: 25.0, w: 1440.0, h: 875.0 };
        let huge = ScreenRect { x: 0.0, y: 25.0, w: 1440.0, h: 870.0 }; // ~entire window
        assert!(is_shallow_pick("AXGroup", &huge, ScreenPoint { x: 150.0, y: 400.0 }, Some(&win), 0));
    }

    #[test]
    fn rich_window_sized_native_group_is_not_shallow() {
        // The Calendar bug: "Year Calendar Area" fills the window but holds the 12
        // month grids — a legitimate pick. Must NOT be treated as shallow (which
        // would shrink it to a tiny click-centered box on commit).
        let win = ScreenRect { x: 0.0, y: 25.0, w: 935.0, h: 598.0 };
        let area = ScreenRect { x: 0.0, y: 25.0, w: 935.0, h: 595.0 }; // ~entire window
        assert!(!is_shallow_pick("AXGroup", &area, ScreenPoint { x: 400.0, y: 300.0 }, Some(&win), 12));
    }
}
