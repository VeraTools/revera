`vera stats | head -1` (or any command piped into `head`) printed a Rust panic once the reader closed the pipe:

```
thread 'main' panicked at library/std/src/io/stdio.rs: failed printing to stdout: Broken pipe (os error 32)
```

Agents pipe Vera output into `head` all the time, and the panic text reads like a crash. Rust ignores SIGPIPE by default, so every `println!` to a closed pipe panics. On Unix, this restores the default SIGPIPE disposition for normal commands, so they exit the way `grep` or `ls` do (status 141, no message).

`vera mcp` and `vera serve` keep Rust's default, so a client that disconnects mid-write cannot kill a server. Windows has no SIGPIPE and is unchanged.

Test: `crates/vera-cli/tests/broken_pipe.rs` runs `vera config` with stdout already closed and checks for no panic and a SIGPIPE exit.

`cargo deny` fails on master until #303 lands (tokio advisory).
