fn main() -> bake::Result<()> {
    bake::Registry::discover()?.run()
}

#[path = "bake_generated_tasks/mod.rs"]
mod bake_generated_tasks;
