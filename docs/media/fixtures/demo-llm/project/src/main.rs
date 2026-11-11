fn main() {
    let names = vec!["ada", "grace", "linus"];
    let greeting = String::from("hello");

    let shout = move || println!("{greeting}!");
    shout();

    // Say hello to everyone on the list, one per line,
    // in the order they were given.
    for name in &names {
        println!("{greeting}, {name}");
    }
}
