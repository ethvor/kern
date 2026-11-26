<!-- kern:managed:start -->
# Project Orientation

This repository uses `kern` for canonical context delivery.

1. Run `kern bootstrap` (or call the `kern_bootstrap` MCP tool) from anywhere inside this repository.
2. If the response ends with `NEXT REQUIRED: run 'kern bootstrap --page N'`, read all remaining pages until `COMPLETE`.
3. Fetch `On Demand` documents when their trigger applies using `kern read <NAME>` (or `kern_read`). Use `kern search <query>` to find relevant guidance.
4. After context compaction, run `kern bootstrap` again before context-dependent work.
<!-- kern:managed:end -->
<!-- kern:local:start -->
<!-- kern:local:end -->
