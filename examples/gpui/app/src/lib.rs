/*
 * Copyright (c) Meta Platforms, Inc. and affiliates.
 *
 * This source code is dual-licensed under either the MIT license found in the
 * LICENSE-MIT file in the root directory of this source tree or the Apache
 * License, Version 2.0 found in the LICENSE-APACHE file in the root directory
 * of this source tree. You may select, at your option, one of the
 * above-listed licenses.
 */

//! A gpui window that `buck2 dev` patches while it runs. Edit `view` and save: the window draws
//! the new code, and keeps its count.

use std::io::BufRead;

use futures::StreamExt;
use gpui::App;
use gpui::Application;
use gpui::Bounds;
use gpui::Context;
use gpui::Div;
use gpui::Window;
use gpui::WindowBounds;
use gpui::WindowOptions;
use gpui::div;
use gpui::prelude::*;
use gpui::px;
use gpui::rgb;
use gpui::size;

struct Counter {
    count: u64,
}

impl Render for Counter {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // Runs the newest patched version of `view`.
        subsecond::HotFn::current(view).call((self, window, cx))
    }
}

fn view(counter: &mut Counter, _window: &mut Window, cx: &mut Context<Counter>) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_4()
        .size_full()
        .justify_center()
        .items_center()
        .bg(rgb(0x1e1e2e))
        .text_xl()
        .text_color(rgb(0xcdd6f4))
        .child(format!("Clicked {} times", counter.count))
        .child(
            div()
                .id("click")
                .px_4()
                .py_2()
                .rounded_md()
                .bg(rgb(0x89b4fa))
                .text_color(rgb(0x1e1e2e))
                .cursor_pointer()
                .child("Click me")
                .on_click(cx.listener(|counter, _, _, cx| {
                    counter.count += 1;
                    cx.notify();
                })),
        )
}

pub fn run() {
    // The server writes one jump table per line to our stdin. They are applied on gpui's main
    // thread, between two frames, and not while a window renders.
    let (tables, mut patches) = futures::channel::mpsc::unbounded();
    std::thread::spawn(move || {
        for line in std::io::stdin().lock().lines() {
            let Ok(line) = line else { break };
            match serde_json::from_str::<subsecond::JumpTable>(&line) {
                Ok(table) => {
                    if tables.unbounded_send(table).is_err() {
                        break;
                    }
                }
                Err(e) => eprintln!("[app] bad jump table: {e}"),
            }
        }
    });

    Application::new().run(|cx: &mut App| {
        let bounds = Bounds::centered(None, size(px(500.), px(500.)), cx);
        cx.open_window(
            WindowOptions {
                window_bounds: Some(WindowBounds::Windowed(bounds)),
                ..Default::default()
            },
            |_, cx| cx.new(|_| Counter { count: 0 }),
        )
        .unwrap();
        cx.on_window_closed(|cx| {
            if cx.windows().is_empty() {
                cx.quit();
            }
        })
        .detach();

        cx.spawn(async move |cx| {
            while let Some(table) = patches.next().await {
                // SAFETY: the server built the patch from this crate, against this binary.
                match unsafe { subsecond::apply_patch(table) } {
                    Ok(()) => eprintln!("[app] patched"),
                    Err(e) => eprintln!("[app] could not apply the patch: {e}"),
                }
                // gpui draws a window again only when something in it changes. Draw them all,
                // with the new code.
                if cx.update(|cx| cx.refresh_windows()).is_err() {
                    break;
                }
            }
        })
        .detach();
        cx.activate(true);
    });
}
