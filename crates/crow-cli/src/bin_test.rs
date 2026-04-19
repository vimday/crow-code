use std::io::{stdout, Write};
use crossterm::{execute, terminal::{size}, cursor::{MoveTo, SavePosition, RestorePosition}};

fn main() {
    let (cols, rows) = size().unwrap();
    print!("\x1B[1;{}r", rows - 1);
    stdout().flush().unwrap();
    execute!(stdout(), SavePosition, MoveTo(0, rows - 1), crossterm::style::Print(format!("{:width$}", " BOTTOM STATUS BAR ", width = cols as usize)), RestorePosition).unwrap();
    
    for i in 1..=10 {
        println!("Line {}", i);
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    print!("\x1B[r");
    stdout().flush().unwrap();
}
