#pragma once

#include "transcribe/azure.hpp"

#include <nlohmann/json.hpp>

#include <stop_token>
#include <string>
#include <string_view>
#include <utility>
#include <vector>

namespace transcribe::azure {

// Selected per request, so the resource needs no deployment.
inline constexpr const char* MODEL = "mai-transcribe-2-streaming";
inline constexpr const char* FAST_MODEL = "MAI-Transcribe-2";

// The region of the first (region, issueToken URL) probe that accepts `key`.
std::string find_region(const std::string& key,
                        const std::vector<std::pair<std::string, std::string>>& probes);

Stream connect_to(const std::string& url, const std::string& key, std::stop_token stop);
std::pair<std::string, double> transcribe_at(const std::string& url, const std::string& key,
                                             audio::Receiver audio, bool clean,
                                             std::stop_token stop);

// The path and JSON body of a service message.
std::pair<std::string, nlohmann::json> parse(std::string_view text);

} // namespace transcribe::azure
