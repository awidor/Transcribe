# Transcribe

## Product writing

Use labels, controls, and status/error messages only. Do not add subtitles, explanatory sentences, helper copy, or instructional placeholders.

## Before publishing

Test the installed release on each target OS with physical shortcuts and a real microphone → transcription → insertion session. Include the user's actual editors and terminals, multiple monitors, and macOS Spaces/full-screen apps. Automated fixtures do not establish compatibility with those environments. Publish the GitHub draft only after these checks pass.

## macOS permission repair

After migrating from an ad-hoc build to a certificate-signed installation, an old Accessibility/Input Monitoring grant can remain tied to the previous executable. Remove the stale Transcribe entry from the affected System Settings privacy pane, add the installed app again, and reopen it. Grant persistent access to the installed app, not a build-directory copy. On newer macOS versions, Accessibility may be labeled **Device Control and Data Access**.
