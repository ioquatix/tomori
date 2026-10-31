# Agent instructions

Run `cargo bake agent:context:install` to install dependency context and skills. Follow the Base conventions and relevant installed skills under `.agents/`.

Before planning or changing behavior, discover and read applicable `design/*.md`. Use `base-design-review` to assess plans and changes, and `base-design-author` when user requirements are introduced, changed, or removed. Review design compliance before opening or updating a pull request.

Development tooling belongs in the private `bake/` package. The root `tomori` package is the headset and desktop application; keep agent backends, protocol types and the PC server in separate workspace crates under `crates/`.
