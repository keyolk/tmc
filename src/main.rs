mod collect;

use anyhow::Result;

fn main() -> Result<()> {
    let panes = collect::tmux::panes()?;
    let tree = collect::proc::Tree::capture()?;
    let confidence = collect::tmux::session_confidence(&panes, &tree);
    for (p, c) in panes.iter().zip(confidence) {
        println!("{:<20} {:<10} {:?}", p.target(), p.current_command, c);
    }
    Ok(())
}
