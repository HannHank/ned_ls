# Changelog

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

## [0.3.0]

### Added

- INI: warning when a parameter is assigned in NED without `default()`, since OMNeT++ ignores the INI value in that case.
- Hover shows the documentation of parameters, gates and submodules, including trailing comments.

### Fixed

- NED: parametric channel types (`<chanType> like IChannel`) no longer report the parameter name as an unknown type.
