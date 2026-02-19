// Copyright 2019-2024 Tauri Programme within The Commons Conservancy
// SPDX-License-Identifier: Apache-2.0
// SPDX-License-Identifier: MIT

pub fn error<S: AsRef<str>>(_err: S) {
    // TODO: implement error dialog for Linux (e.g. via GTK message dialog)
}
