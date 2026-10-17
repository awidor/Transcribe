#pragma once

#include <string>

namespace transcribe::insertion {

// Pastes `text` into the app and field in front at this moment. Each id is attempted once.
void insert(const std::string& id, const std::string& text);
void copy(const std::string& text);
// Chromium and Electron build their accessibility trees only after a client asks. Asking
// when recording starts lets delivery recognize their text fields.
void prepare();

} // namespace transcribe::insertion
