#pragma once

#include "transcribe/provider.hpp"

#include <string_view>
#include <vector>

namespace transcribe::provider {

// OpenRouter's model list as cleanup models with the reasoning efforts each accepts.
std::vector<CleanupModel> normalize_catalog(std::string_view bytes);

} // namespace transcribe::provider
