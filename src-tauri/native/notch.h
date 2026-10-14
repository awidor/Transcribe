#pragma once
#include <stdbool.h>
#include <stdint.h>

// All calls except tc_notch_level run on the AppKit main thread.
// Actions: 0 = stop, 1 = cancel/dismiss, 2 = open history.
typedef void (*TCNotchAction)(int action);
void tc_notch_init(TCNotchAction action);
// text is confirmed speech; interim is the guess that follows it.
void tc_notch_update(const char *phase, int64_t started_at, const char *error,
                     const char *text, const char *interim);
void tc_notch_level(float level);
void tc_notch_hide(void);
void tc_notch_destroy(void);
