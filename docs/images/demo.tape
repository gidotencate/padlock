# Optional: render the terminal demo with a real terminal instead of generate.py
#   vhs docs/images/demo.tape      (https://github.com/charmbracelet/vhs)
Output docs/images/demo-terminal-vhs.gif
Set FontSize 16
Set Width 1100
Set Height 600
Set Theme "Dracula"
Hide
Type "mkdir -p /tmp/padlock-demo && cd /tmp/padlock-demo" Enter
Type "printf '#[repr(C)]\npub struct Connection {\n    pub active: bool,\n    pub id: u64,\n    pub retries: u8,\n    pub timeout_ms: u32,\n    pub port: u16,\n}\n' > connection.rs" Enter
Type "clear" Enter
Show
Type "padlock analyze connection.rs" Enter
Sleep 3s
Type "padlock fix connection.rs" Enter
Sleep 3s
Type "padlock analyze connection.rs" Enter
Sleep 4s
