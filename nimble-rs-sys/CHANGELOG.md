# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## Unreleased
* (Breaking) Link the host's `os_mempool` implementation under a private `nimble_rs_` prefix (glue `os/os_mempool.h`): esp-radio exports the unprefixed names for the ESP32-C6/H2/C2 controller blob, with a different `struct os_mempool` layout - a duplicate-symbol link error in debug builds, a misplaced extended-pool callback in release builds

## [0.1.0] - 2026-08-20
* Initial release
