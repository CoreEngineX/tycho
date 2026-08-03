//! Layer 1. The one chokepoint for spawning git, plus the profile lock, atomic file
//! writes, and `st_mode` classification. No other module spawns a process, which is
//! what makes the pinned config and the per-child timeout unavoidable rather than
//! remembered.
