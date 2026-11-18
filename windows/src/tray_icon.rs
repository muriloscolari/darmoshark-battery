// SPDX-License-Identifier: GPL-2.0-or-later
//! Native-looking transparent taskbar widget renderer (hollow mouse + Segoe UI percentage).

use std::mem::{size_of, zeroed};
use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HWND, POINT, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CreateCompatibleDC, CreateDIBSection, CreateFontW, DeleteDC, DeleteObject, DrawTextW,
    GdiFlush, SelectObject, SetBkMode, SetTextColor, AC_SRC_ALPHA, AC_SRC_OVER, BITMAPINFO,
    BITMAPINFOHEADER, BI_RGB, BLENDFUNCTION, CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS,
    DEFAULT_CHARSET, DIB_RGB_COLORS, DT_CALCRECT, DT_LEFT, DT_SINGLELINE, DT_VCENTER, FW_NORMAL,
    OUT_DEFAULT_PRECIS, TRANSPARENT,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{UpdateLayeredWindow, ULW_ALPHA};

fn wide(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// Signed distance to a rounded box centered at (0, 0) with half-extents (bx, by) and radius r.
fn sd_rounded_box(px: f32, py: f32, bx: f32, by: f32, r: f32) -> f32 {
    let qx = px.abs() - (bx - r);
    let qy = py.abs() - (by - r);
    let out = (qx.max(0.0).powi(2) + qy.max(0.0).powi(2)).sqrt();
    let inside = qx.max(qy).min(0.0);
    out + inside - r
}

/// Signed distance to a vertical capsule from (0, y0) to (0, y1) with radius r.
fn sd_vsegment(px: f32, py: f32, y0: f32, y1: f32, r: f32) -> f32 {
    let cy = py.clamp(y0, y1);
    (px * px + (py - cy) * (py - cy)).sqrt() - r
}

/// Render the hollow mouse + percentage directly into `hwnd` via `UpdateLayeredWindow`.
/// Returns the rendered `(width, height)` in pixels.
pub fn update_widget_surface(
    hwnd: HWND,
    pos: POINT,
    taskbar_height: i32,
    dpi: u32,
    percent: Option<u8>,
    light_taskbar: bool,
) -> Option<(i32, i32)> {
    let scale = (dpi as f32 / 96.0).max(1.0);
    let h = taskbar_height.max((24.0 * scale) as i32);
    let small_bar = h < (36.0 * scale).round() as i32;

    let label = match percent {
        Some(p) => format!("{p}%"),
        None => "--".to_string(),
    };
    let wide_label = wide(&label);

    // Adapt typography & icon size for thin ("small taskbar buttons") vs normal taskbar.
    let font_px = if small_bar {
        -(11.0 * scale).round() as i32
    } else {
        -(12.0 * scale).round() as i32
    };
    let font_weight = if small_bar { 500 } else { FW_NORMAL as i32 };
    let font_name = wide("Segoe UI");

    let hdc_screen = null_mut();
    let hdc_mem = unsafe { CreateCompatibleDC(hdc_screen) };
    if hdc_mem.is_null() {
        return None;
    }

    let hfont = unsafe {
        CreateFontW(
            font_px,
            0,
            0,
            0,
            font_weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            0,
            font_name.as_ptr(),
        )
    };
    let old_font = if !hfont.is_null() {
        unsafe { SelectObject(hdc_mem, hfont) }
    } else {
        null_mut()
    };

    // Measure text width.
    let mut calc_rc = RECT {
        left: 0,
        top: 0,
        right: 0,
        bottom: h,
    };
    unsafe {
        DrawTextW(
            hdc_mem,
            wide_label.as_ptr(),
            (wide_label.len() - 1) as i32,
            &mut calc_rc,
            DT_CALCRECT | DT_LEFT | DT_SINGLELINE,
        );
    }

    let pad_x = (5.0 * scale).round() as i32;
    let mouse_box = if small_bar {
        (14.0 * scale).round() as i32
    } else {
        (16.0 * scale).round() as i32
    };
    let gap = (4.0 * scale).round() as i32;
    let text_w = (calc_rc.right - calc_rc.left).max(12);
    let w = pad_x + mouse_box + gap + text_w + pad_x;

    let mut bmi: BITMAPINFO = unsafe { zeroed() };
    bmi.bmiHeader.biSize = size_of::<BITMAPINFOHEADER>() as u32;
    bmi.bmiHeader.biWidth = w;
    bmi.bmiHeader.biHeight = -h; // top-down
    bmi.bmiHeader.biPlanes = 1;
    bmi.bmiHeader.biBitCount = 32;
    bmi.bmiHeader.biCompression = BI_RGB;

    let mut bits_ptr: *mut std::ffi::c_void = null_mut();
    let hbm = unsafe {
        CreateDIBSection(
            hdc_mem,
            &bmi,
            DIB_RGB_COLORS,
            &mut bits_ptr,
            null_mut(),
            0,
        )
    };
    if hbm.is_null() || bits_ptr.is_null() {
        if !old_font.is_null() {
            unsafe { SelectObject(hdc_mem, old_font) };
        }
        if !hfont.is_null() {
            unsafe { DeleteObject(hfont) };
        }
        unsafe { DeleteDC(hdc_mem) };
        return None;
    }

    let old_bm = unsafe { SelectObject(hdc_mem, hbm) };
    let pixels = unsafe { std::slice::from_raw_parts_mut(bits_ptr as *mut u32, (w * h) as usize) };
    pixels.fill(0);

    // Draw white text on zeroed black DIB so GDI writes grayscale coverage in RGB.
    let text_x = pad_x + mouse_box + gap;
    let mut text_rc = RECT {
        left: text_x,
        top: 0,
        right: w - pad_x,
        bottom: h,
    };
    unsafe {
        SetBkMode(hdc_mem, TRANSPARENT as i32);
        SetTextColor(hdc_mem, 0x00ff_ffff);
        DrawTextW(
            hdc_mem,
            wide_label.as_ptr(),
            (wide_label.len() - 1) as i32,
            &mut text_rc,
            DT_LEFT | DT_VCENTER | DT_SINGLELINE,
        );
        GdiFlush();
    }

    let (fg_r, fg_g, fg_b) = match percent {
        Some(p) if p <= 15 => (0xef_u32, 0x44_u32, 0x44_u32),
        _ if light_taskbar => (0x18_u32, 0x18_u32, 0x1b_u32),
        _ => (0xff_u32, 0xff_u32, 0xff_u32),
    };
    let dim_mul = if percent.is_some() { 255_u32 } else { 150_u32 };

    // Convert ClearType RGB coverage into gamma-corrected premultiplied ARGB.
    for y in 0..h {
        for x in text_x..w {
            let idx = (y * w + x) as usize;
            let px = pixels[idx];
            let r_cov = (px >> 16) & 0xff;
            let g_cov = (px >> 8) & 0xff;
            let b_cov = px & 0xff;
            let cov = (r_cov + g_cov * 2 + b_cov) / 4;
            if cov > 0 {
                let norm = (cov as f32 / 255.0).powf(0.68);
                let a = ((norm * dim_mul as f32).round() as u32).min(255);
                let r = (fg_r * a) / 255;
                let g = (fg_g * a) / 255;
                let b = (fg_b * a) / 255;
                pixels[idx] = (a << 24) | (r << 16) | (g << 8) | b;
            }
        }
    }

    // Render anti-aliased hollow mouse icon using analytical SDF over a 2x2 subpixel grid.
    let mx0 = pad_x;
    let my0 = (h - mouse_box) / 2;
    let cx = mx0 as f32 + mouse_box as f32 * 0.5;
    let cy = my0 as f32 + mouse_box as f32 * 0.5;
    let body_bx = mouse_box as f32 * 0.34;
    let body_by = mouse_box as f32 * 0.45;
    let body_r = body_bx * 0.92;
    let stroke = (1.35 * scale).max(1.1);
    let wheel_r = (0.65 * scale).max(0.55);

    for y in (my0 - 1).max(0)..(my0 + mouse_box + 1).min(h) {
        for x in (mx0 - 1).max(0)..(mx0 + mouse_box + 1).min(w) {
            let mut cov_sum = 0.0f32;
            for sy in 0..2 {
                for sx in 0..2 {
                    let px = x as f32 + (sx as f32 + 0.5) * 0.5 - cx;
                    let py = y as f32 + (sy as f32 + 0.5) * 0.5 - cy;

                    let d_body = sd_rounded_box(px, py, body_bx, body_by, body_r);
                    let d_outline = d_body.abs() - stroke * 0.5;
                    let d_wheel = sd_vsegment(
                        px,
                        py,
                        -body_by * 0.52,
                        -body_by * 0.16,
                        wheel_r,
                    );
                    let d = d_outline.min(d_wheel);
                    cov_sum += (0.5 - d).clamp(0.0, 1.0);
                }
            }
            let cov = (cov_sum * 0.25).powf(0.85);
            if cov > 0.0 {
                let a = ((cov * dim_mul as f32).round() as u32).min(255);
                if a > 0 {
                    let r = (fg_r * a) / 255;
                    let g = (fg_g * a) / 255;
                    let b = (fg_b * a) / 255;
                    pixels[(y * w + x) as usize] = (a << 24) | (r << 16) | (g << 8) | b;
                }
            }
        }
    }

    // Ensure at least alpha=1 across the widget rect so mouse hover/clicks hit it cleanly.
    for px in pixels.iter_mut() {
        if (*px >> 24) == 0 {
            *px = 0x0100_0000;
        }
    }

    let dst_pt = POINT { x: pos.x - w, y: pos.y };
    let size = SIZE { cx: w, cy: h };
    let src_pt = POINT { x: 0, y: 0 };
    let blend = BLENDFUNCTION {
        BlendOp: AC_SRC_OVER as u8,
        BlendFlags: 0,
        SourceConstantAlpha: 255,
        AlphaFormat: AC_SRC_ALPHA as u8,
    };

    unsafe {
        UpdateLayeredWindow(
            hwnd,
            null_mut(),
            &dst_pt,
            &size,
            hdc_mem,
            &src_pt,
            0,
            &blend,
            ULW_ALPHA,
        );
        SelectObject(hdc_mem, old_bm);
        DeleteObject(hbm);
        if !old_font.is_null() {
            SelectObject(hdc_mem, old_font);
        }
        if !hfont.is_null() {
            DeleteObject(hfont);
        }
        DeleteDC(hdc_mem);
    }

    Some((w, h))
}
