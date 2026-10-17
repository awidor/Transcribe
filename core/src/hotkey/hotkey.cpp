#include "hotkey/engine.hpp"

#include <algorithm>
#include <charconv>
#include <limits>

namespace transcribe::hotkey {

void to_json(nlohmann::json& json, const Key& key) {
    json = {{"code", key.code}, {"label", key.label}};
}

void from_json(const nlohmann::json& json, Key& key) {
    const auto& code = json.at("code");
    // serde reads a u32 only from a whole number that fits.
    if (!code.is_number_unsigned() ||
        code.get<std::uint64_t>() > std::numeric_limits<std::uint32_t>::max())
        throw Error("Invalid shortcut");
    key.code = code.get<std::uint32_t>();
    key.label = json.at("label").get<std::string>();
}

void to_json(nlohmann::json& json, const Binding& binding) {
    json = {{"platform", binding.platform}, {"keys", binding.keys}};
}

void from_json(const nlohmann::json& json, Binding& binding) {
    binding.platform = json.at("platform").get<std::string>();
    binding.keys = json.at("keys").get<std::vector<Key>>();
}

namespace {

// serde's field order, since saved settings hold this text.
std::string serialize(const Binding& binding) {
    auto keys = nlohmann::ordered_json::array();
    for (const auto& key : binding.keys)
        keys.push_back({{"code", key.code}, {"label", key.label}});
    nlohmann::ordered_json json = {{"platform", binding.platform}, {"keys", keys}};
    return json.dump(-1, ' ', false, nlohmann::ordered_json::error_handler_t::replace);
}

} // namespace

// Escape on its own ends a dismissible session, unless the shortcut uses it.
bool Engine::dismisses(std::uint32_t key) const {
    auto uses_escape = std::ranges::any_of(binding, [&](const auto& options) {
        return std::ranges::any_of(options, [&](auto k) { return escape.contains(k); });
    });
    return dismissible && !paused && held.size() == 1 && escape.contains(key) && !uses_escape;
}

bool Engine::matches() const {
    return !binding.empty() && stroke.size() == binding.size() &&
           std::ranges::all_of(binding, [&](const auto& options) {
               return std::ranges::any_of(options, [&](auto k) { return stroke.contains(k); });
           });
}

void Engine::reset() {
    stroke.clear();
    releasing = false;
    // Never activate from the tail of a chord held while settings changed.
    invalid = !held.empty();
}

#ifdef _WIN32
// A release the listener never saw would otherwise keep the key held and
// block every later chord.
void Engine::forget(const std::set<std::uint32_t>& released) {
    std::erase_if(held, [&](auto key) { return released.contains(key); });
    std::erase_if(swallowed, [&](auto key) { return released.contains(key); });
    std::erase_if(masked, [&](auto key) { return released.contains(key); });
    std::erase_if(stroke, [&](auto key) { return released.contains(key); });
    if (held.empty()) {
        if (capture) capture->armed = true;
        reset();
    }
}
#endif

std::optional<Event> Engine::cancel() {
    if (!capture) return std::nullopt;
    auto token = capture->token;
    capture.reset();
    activation_inhibit_until = Clock::now() + std::chrono::milliseconds(250);
    reset();
    return Cancelled{token};
}

std::pair<bool, std::vector<Event>> Engine::input(Key key, bool down, bool modifier) {
    std::vector<Event> events;
    if (capture && capture->deadline <= Clock::now()) {
        if (auto event = cancel()) events.push_back(std::move(*event));
    }
    bool capturing = capture.has_value();
    bool swallowed = this->swallowed.contains(key.code);
    bool dismiss = false;
    // A keyboard-activated button may start capture before its Enter/Space
    // key-up. Drain that initiating chord without treating it as a binding.
    if (capture && !capture->armed) {
        if (down) {
            held.insert(key.code);
        } else {
            held.erase(key.code);
            this->swallowed.erase(key.code);
        }
        if (held.empty()) {
            capture->armed = true;
            reset();
        }
        return {swallowed, std::move(events)};
    }
    if (down) {
        if (!held.insert(key.code).second) return {capturing || swallowed, std::move(events)};
        if (releasing) invalid = true;
        stroke.insert(key.code);
        if (capture) {
            capture->keys.push_back(key);
            events.push_back(hotkey::Capture{capture->token, capture->keys, std::nullopt});
        } else if (dismisses(key.code)) {
            dismiss = true;
            invalid = true;
            events.push_back(Dismiss{});
        } else if (matches()) {
            // The shortcut's Alt or Windows key must never also open a menu
            // and take focus from the destination field, even while the
            // shortcut itself is ignored, such as during a paste.
            for (auto k : stroke)
                if (menu_keys.contains(k)) masked.insert(k);
        }
    } else {
        if (!held.erase(key.code)) return {swallowed, std::move(events)};
        this->swallowed.erase(key.code);
        releasing = true;
        if (masked.erase(key.code)) mask_release = key.code;
    }
    // Modifiers must remain usable for normal typing and other shortcuts.
    bool suppress = capturing || swallowed || dismiss || (!down && mask_release == key.code) ||
                    (down && !modifier && !paused && !invalid && matches());
    if (down && suppress) this->swallowed.insert(key.code);
    if (held.empty()) {
        if (capture) {
            auto finished = std::move(*capture);
            capture.reset();
            if (!invalid && !finished.keys.empty()) {
                auto shortcut = serialize({platform(), finished.keys});
                events.push_back(
                    hotkey::Capture{finished.token, std::move(finished.keys), shortcut});
            } else {
                events.push_back(Cancelled{finished.token});
            }
        } else if (!paused && !invalid && matches()) {
            events.push_back(Activate{});
        }
        reset();
    }
    return {suppress, std::move(events)};
}

std::string platform() {
#ifdef _WIN32
    return "windows";
#else
    return is_wayland() ? "linux-evdev" : "linux";
#endif
}

std::vector<std::string> split(std::string_view text, char separator) {
    std::vector<std::string> pieces;
    for (;;) {
        auto end = text.find(separator);
        pieces.emplace_back(text.substr(0, end));
        if (end == std::string_view::npos) return pieces;
        text.remove_prefix(end + 1);
    }
}

std::optional<std::uint32_t> parse_u32(std::string_view text) {
    if (text.starts_with('+')) text.remove_prefix(1);
    std::uint32_t value = 0;
    auto [end, error] = std::from_chars(text.data(), text.data() + text.size(), value);
    if (text.empty() || error != std::errc() || end != text.data() + text.size())
        return std::nullopt;
    return value;
}

Binding parse(const std::string& text) {
    try {
        return nlohmann::json::parse(text).get<Binding>();
    } catch (const nlohmann::json::exception&) {
        throw Error("Invalid shortcut");
    }
}

Service::State::State(std::function<void(const Event&)> handler)
    : sender(std::make_shared<Channel<Event>>(std::numeric_limits<std::size_t>::max())) {
    this->handler = std::jthread([receiver = sender, handler = std::move(handler)] {
        while (auto event = receiver->recv())
            handler(*event);
    });
}

void Service::State::send(Event event) {
    sender->send(std::move(event));
}

bool Service::State::input(std::uint32_t code, std::string label, bool down, bool modifier) {
    std::unique_lock lock(mutex);
    auto [suppress, events] = engine.input(Key{code, std::move(label)}, down, modifier);
    lock.unlock();
    for (auto& event : events)
        send(std::move(event));
    return suppress;
}

void Service::State::failed(const std::string& message) {
    {
        std::lock_guard lock(start_mutex);
        started = false;
    }
    std::lock_guard lock(mutex);
    if (auto event = engine.cancel()) send(std::move(*event));
    engine.held.clear();
    engine.swallowed.clear();
    engine.masked.clear();
    engine.reset();
    send(Failure{message});
}

void Service::State::activate() {
    std::lock_guard lock(mutex);
    if (!engine.paused && !engine.capture &&
        (!engine.activation_inhibit_until || *engine.activation_inhibit_until <= Clock::now()))
        send(Activate{});
}

#ifdef __linux__
void Service::State::reset_input() {
    std::lock_guard lock(mutex);
    if (auto event = engine.cancel()) send(std::move(*event));
    engine.held.clear();
    engine.swallowed.clear();
    engine.masked.clear();
    engine.reset();
}
#endif

#ifdef _WIN32
std::optional<std::uint32_t> Service::State::take_mask() {
    std::lock_guard lock(mutex);
    return std::exchange(engine.mask_release, std::nullopt);
}

void Service::State::window_input(Key key, bool down, bool (*pressed)(std::uint32_t)) {
    // A report can arrive after the listener already saw the release; replaying
    // that press would activate twice.
    if (down && !pressed(key.code)) return;
    {
        std::lock_guard lock(mutex);
        if (engine.capture) return;
    }
    if (down) forget_released(key.code, pressed);
    std::unique_lock lock(mutex);
    auto modifier = native::modifier(key.code);
    auto events = engine.input(std::move(key), down, modifier).second;
    lock.unlock();
    for (auto& event : events)
        send(std::move(event));
}

void Service::State::forget_released(std::uint32_t pressing, bool (*pressed)(std::uint32_t)) {
    std::set<std::uint32_t> held;
    {
        std::lock_guard lock(mutex);
        held = engine.held;
    }
    // Query outside the lock, like every other native call here.
    std::set<std::uint32_t> released;
    for (auto key : held)
        if (key != pressing && !pressed(key)) released.insert(key);
    if (!released.empty()) {
        std::lock_guard lock(mutex);
        engine.forget(released);
    }
}
#endif

std::shared_ptr<Service> Service::create(std::function<void(const Event&)> handler) {
    return std::shared_ptr<Service>(new Service(std::make_shared<State>(std::move(handler))));
}

Service::Service(std::shared_ptr<State> state) : state_(std::move(state)) {}

Service::~Service() {
    // Rust keeps the listener for the life of the process; here it ends with the service,
    // and events after this point are dropped while the handler finishes the queued ones.
    state_->sender->close();
    state_->listener = std::jthread();
    // A handler that drops the last reference cannot wait for itself.
    if (state_->handler.get_id() == std::this_thread::get_id()) state_->handler.detach();
}

void Service::start() {
    std::lock_guard lock(state_->start_mutex);
    if (!state_->started) {
        native::start(*state_);
        state_->started = true;
    }
}

Chord Service::validate(const std::string& text) {
#ifdef __linux__
    if (platform() == "linux-evdev") return evdev::binding(text);
#endif
    if (text.starts_with('{')) {
        auto binding = parse(text);
        if (binding.platform != platform()) throw Error("Shortcut belongs to another platform");
        if (binding.keys.empty() || binding.keys.size() > 512) throw Error("Invalid shortcut");
        std::set<std::uint32_t> keys;
        for (const auto& key : binding.keys)
            keys.insert(key.code);
        if (keys.size() != binding.keys.size()) throw Error("Duplicate shortcut key");
        native::validate(keys);
        Chord chord;
        for (auto key : keys)
            chord.push_back({key});
        return chord;
    }
    auto binding = native::legacy(text);
    std::set<std::uint32_t> seen;
    for (const auto& alternatives : binding)
        for (auto key : alternatives)
            if (!seen.insert(key).second) throw Error("Duplicate shortcut key");
    native::validate(seen);
    return binding;
}

void Service::configure(Chord binding) {
    std::lock_guard lock(state_->mutex);
    state_->engine.binding = std::move(binding);
    state_->engine.reset();
}

std::uint64_t Service::begin_capture() {
    start();
#ifdef _WIN32
    native::prepare_capture(*state_);
#endif
    std::lock_guard lock(state_->mutex);
    auto& engine = state_->engine;
    engine.cancel();
    engine.reset();
    auto token = ++engine.next_token;
    engine.capture =
        Engine::Capture{token, Clock::now() + std::chrono::seconds(15), {}, engine.held.empty()};
    return token;
}

void Service::cancel_capture(std::optional<std::uint64_t> token) {
    std::lock_guard lock(state_->mutex);
    auto& engine = state_->engine;
    if (!token || (engine.capture && engine.capture->token == *token)) {
        if (auto event = engine.cancel()) state_->send(std::move(*event));
    }
}

// Focused-window input is scoped to an explicit capture; it can never
// activate a global binding or revive an expired/cancelled recorder.
std::vector<Event> Service::capture_key(std::uint64_t token, const Key& key, bool down) {
#ifdef _WIN32
    native::validate({key.code});
    std::lock_guard lock(state_->mutex);
    auto& engine = state_->engine;
    if (!engine.capture || engine.capture->token != token) return {};
    if (engine.capture->deadline <= Clock::now()) {
        std::vector<Event> events;
        if (auto event = engine.cancel()) events.push_back(std::move(*event));
        return events;
    }
    auto modifier = native::modifier(key.code);
    return engine.input(key, down, modifier).second;
#else
    (void)token, (void)key, (void)down;
    throw Error("Focused-window capture is unavailable on this platform");
#endif
}

Chord Service::binding() const {
    std::lock_guard lock(state_->mutex);
    return state_->engine.binding;
}

void Service::window_key(const Key& key, bool down) {
#ifdef _WIN32
    native::validate({key.code});
    state_->window_input(key, down, native::pressed);
#else
    (void)key, (void)down;
    throw Error("Window shortcut input is unavailable on this platform");
#endif
}

void Service::paused(bool paused) {
    std::lock_guard lock(state_->mutex);
    state_->engine.paused = paused;
    state_->engine.reset();
}

Service::Pause Service::pause() {
    paused(true);
    return Pause(this);
}

void Service::dismissible(bool dismissible) {
    std::lock_guard lock(state_->mutex);
    state_->engine.dismissible = dismissible;
}

void Service::activate() {
    state_->activate();
}

} // namespace transcribe::hotkey
