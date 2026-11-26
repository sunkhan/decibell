// Hand-written edge suite, one group per feature. Inputs and the renderer's records are committed
// in full (edge.jsonl); `urlCases` also record parseInviteLink / isInviteLink / mediaLabelFor /
// isGifUrl applied to the input itself.
const groups = {};
const add = (name, ...xs) => { (groups[name] ??= []).push(...xs.flat()); };

// ── emphasis, nesting, unclosed markers, markers inside words ──
add("emphasis",
  "**bold**", "*it*", "_it_", "__u__", "~~s~~", "***bi***", "___ui___", "**bold *it* bold**", "*a **b** c*",
  "**a**b**", "****", "** **", "* a *", "*a *", "* a*", "a*b*c", "a_b_c", "snake_case_name", "_a_b", "a_b_",
  "__init__", "__a__b", "x__a__", "~a~", "~~a~", "~~ a~~", "~~~a~~~", "**a", "a**", "**a*", "*a**", "***a**",
  "**a***", "***a*", "*a***", "_*a*_", "*_a_*", "**_a_**", "__*a*__", "~~**a**~~", "**~~a~~**", "__~~a~~__",
  "*_*_*_*_*_x_*_*_*_*_*", "*".repeat(20), "_".repeat(20), "~".repeat(20), "*a*".repeat(5),
  "**a** **b** **c**", "*a* _b_ __c__ ~~d~~", "**a\n\nb**", "*a\nb*", "_a\nb_", "a\n*b*", "**\n**", "*\n*",
  "(*a*)", "*a*.", "_a_.", "._a_.", "a_b_.", "(_a_)", "1_2_3", "é_a_é", "a_é_", "_é_a", "x_a_", "_a_x",
  "__a__b", "a__b__c", "***a***b", "***a** b*", "* **a** *", "**a *b* c**d", "*a**b**c*", "_a__b__c_",
  "___a___", "___a__", "__a___", "___a_", "_a___", "*__a__*", "__*a*__", "**__a__**", "~~__a__~~",
  "**a**\\*", "\\**a**", "**a\\**", "**a\\\\**", "\\\\**a**", "*a\\*b*", "_a\\_b_", "~~a\\~~b~~",
  "**" + "*a*".repeat(3) + "**",
  // nesting depth limit (8)
  "*".repeat(1) + "_".repeat(1) + "x" + "_" + "*",
  Array.from({ length: 10 }, (_, d) => { let s = "x"; for (let k = 0; k < d; k++) s = (k % 2 ? "*" : "_") + s + (k % 2 ? "*" : "_"); return s; }),
  Array.from({ length: 10 }, (_, d) => { let s = "x"; for (let k = 0; k < d; k++) s = "**" + s + "**"; return s; }),
  Array.from({ length: 10 }, (_, d) => { let s = "x"; for (let k = 0; k < d; k++) s = "~~" + s + "~~"; return s; }),
  "** a**", "**a **", "__ a__", "__a __", "~~a ~~", "*\ta*", "*a\t*", "*a\u00a0*", "*\u00a0a*", "*\u3000a*",
  "*\u200ba*", "*a\u2028*", "_\u0301_", "*e\u0301*", "**שלום**", "_שלום_", "*a*b", "a*b*", "*a*1", "1*a*",
);

// ── escapes ──
add("escapes",
  "\\*a\\*", "\\_a\\_", "\\~~a~~", "\\`a`", "\\$a$", "\\\\", "\\", "x\\", "\\n", "a\\b", "C:\\Users\\x",
  "\\_\\_", "\\`code\\`", "\\$5", "\\\\*a*", "\\\\\\*a*", "\\*", "\\_", "\\~", "\\$", "\\`", "\\\\\\",
  "\\:smile:", ":smile\\:", ":sm\\_ile:", ":smile\\_cat:", "\\*😂", "😂\\*", "\\#", "\\a", "\\<https://x.y>",
  "\\https://x.y", "https://x.y\\", "\\```x```", "```x\\```y```", "`a\\`b`", "``a\\``b``", "$a\\$b$",
  "$$a\\$$b$$", "**a\\**b**", "\\\\`a`", "\\\\$a$", "\\*\\*a\\*\\*", "*\\**", "**\\***", "_\\__",
);

// ── inline code ──
add("code",
  "`a`", "``a``", "``a`b``", "`a``b`", "` `", "``", "`a", "a`b`c", "`*a*`", "`**a**`", "`$x$`",
  "`https://x.y`", "`a\nb`", "`\na\n`", "``\na\n``", "`\n`", "x`\n`y", "`:smile:` :smile:", "`😂`",
  "`a` `b`", "```a``", "``a```", "`a\\`", "``` ```", "`` ` ``", "`` `` ``", "`a`b`c`", "``a``b``",
  "x `\n\n` y", "`\r\n`", "`\ta\t`", "a\n`b\nc`\nd", "a\n\n`b\nc`\n\nd", "**`a`**", "*`a`*", "`*a`*",
  "`_a`_", "_`a`_", "~~`a`~~", "`<https://x.y>`", "`decibell://invite/ABCD`",
);

// ── fenced code blocks ──
const langs = ["js", "javascript", "ts", "typescript", "tsx", "jsx", "mjs", "cjs", "mts", "cts", "py", "python",
  "gyp", "ipython", "rs", "rust", "c", "h", "cpp", "cc", "c++", "h++", "hpp", "hh", "hxx", "cxx", "cs", "c#",
  "csharp", "java", "jsp", "kotlin", "kt", "kts", "swift", "go", "golang", "php", "ruby", "rb", "gemspec",
  "podspec", "thor", "irb", "bash", "sh", "zsh", "powershell", "pwsh", "ps", "ps1", "sql", "json", "jsonc",
  "json5", "yaml", "yml", "ini", "toml", "xml", "html", "xhtml", "rss", "atom", "xjb", "xsd", "xsl", "plist",
  "wsf", "svg", "css", "scss", "markdown", "md", "mkdown", "mkd", "dockerfile", "docker", "diff", "patch",
  "lua", "pluto", "plaintext", "text", "txt", "plain", "JS", "Rust", "C#", "TOML", "objective-c", "objc",
  "constructor", "__proto__", "toString", "hasOwnProperty", "foo", "x.y", "a+b", "-", ".", "#", "_", "0",
  "1c", "vb.net", "f#", "shell", "console", "tex", "latex", "haskell", "zig"];
add("fences", langs.map((l) => "```" + l + "\nx = 1\n```"));
add("fences",
  "```\ncode\n```", "```code```", "```js code```", "```lang with space\nx\n```", "```\n\n```", "```\n```",
  "``````", "```a```b```", "text\n```js\nx\n```\nmore", "text\n\n```x```\n\nmore", "```x```\n", "**a```x```b**",
  "a```x```", "```\nx", "```x\n", "```js\nx", "```js\nx\n``` trailing", "```js\r\nx\r\n```", "```js\nx\n\n```",
  "```js\n\nx\n```", "```\nx\n\n\n```", "```js\n```", "```js\n\n```", "``` js\nx\n```", "```js \nx\n```",
  "```js\tx\n```", "a\n```x```\nb", "a\n\n```x```\n\nb", "a\r\n```x```\r\nb", "```x```\n```y```", "```x``````y```",
  "```x```\n\n```y```", "```x``` ```y```", "_a ```x``` b_", "~~```x```~~", "```*a*```", "```https://x.y```",
  "```$x$```", "```:smile:```", "```😂```", "```js\n" + "line\n".repeat(20) + "```", "```" + "x".repeat(300) + "```",
  "```\n```\n```", "````x````", "`````x`````", "```a\n```b\n```", "\n```x```", "\n\n```x```", "```x```\n\n",
  "```x```\n\n\n", " ```x``` ", "a ```x``` b", "```js\nconst s = `template`;\n```", "```js\nx ``` y\n```",
  Array.from({ length: 12 }, (_, i) => "```b" + i + "```").join("\n"),
  Array.from({ length: 12 }, (_, i) => "`m" + i + "\nn`").join(" "),
  Array.from({ length: 12 }, (_, i) => "$x" + i + "$").join(" "),
  Array.from({ length: 6 }, (_, i) => "$$d" + i + "$$\n```c" + i + "```").join("\n"),
  Array.from({ length: 11 }, (_, i) => "**```b" + i + "```**").join(""),
);

// ── math ──
add("math",
  "$x$", "$$x$$", "$ x$", "$x $", "$5 and $10", "$x$5", "$x$ 5", "$x\ny$", "$$ $$", "$$x$", "$x$$", "$$\nx\n$$",
  "a\n$$x$$\nb", "\\$x$", "$x\\$", "$x\\$y$", "$$a$$b$$", "$a$b$", "$$$x$$$", "$$$$", "$\\$", "cost $5", "$5",
  "$x$.", "**$x$**", "$**x**$", "$_x_$", "$a$$b$", "$$a$b$$", "$x$y$z$", "$1$", "$x$1", "$x$a", "$\\frac{a}{b}$",
  "$$\\int_0^1 x^2\\,dx$$", "a $$x$$ b", "a\n\n$$x$$\n\nb", "$$x$$\n", "$$ x $$", "$$\tx\t$$", "$$\u00a0x\u00a0$$",
  "$$\u3000$$", "$\u00a0x$", "$x\u00a0$", "$x\u2028y$", "$x\ry$", "$😂$", "$$😂$$", "$:smile:$", "$https://x.y$",
  "`$x$`", "$`x`$", "$x$`y`", "$$a\nb$$", "$$\n$$", "$$\n\n$$", "x$y$", "($x$)", "$x$)", "1$x$", "$$x$$$$y$$",
  "$$x$$y$$z$$", "$ $", "$$", "$", "$$$", "\\$\\$x$$", "$x$\n$y$", "**$$x$$**", "*$$x$$*", "_$$x$$_",
);

// ── links ──
add("links",
  "https://example.com", "http://example.com", "HTTPS://EXAMPLE.COM", "hTTp://x.y", "https://example.com.",
  "https://example.com,", "https://example.com:", "https://example.com;", "https://example.com!",
  "https://example.com?", "https://example.com'", "https://example.com\"", "(https://example.com)",
  "(see https://x.y/z).", "https://en.wikipedia.org/wiki/Foo_(bar)", "https://en.wikipedia.org/wiki/Foo_(bar))",
  "https://x.y/a]", "https://x.y/[a]", "https://x.y/{a}", "https://x.y/a}", "https://x.y/a)).", "https://x.y/a.,;:!?",
  "https://x.y/a'\"", "https://x.y/'a'", "<https://example.com>", "<https://example.com", "<https://exa mple.com>",
  "< https://x.y>", "<http://x.y>.", "<https://x.y/a b>", "<decibell://invite/ABCD1234>", "<https://x.y>>",
  "<<https://x.y>", "<https://x.y/<a>", "<https://foo>", "<http://>", "https://x.y/*a*", "**https://x.y**",
  "*https://x.y/*", "_https://x.y/_", "~~https://x.y~~", "xhttps://x.y", "1https://x.y", "_https://x.y",
  ".https://x.y", "éhttps://x.y", "https://", "http://", "https://foo", "https://localhost", "https://LOCALHOST",
  "https://localhost:3000/x", "http://127.0.0.1", "http://1234", "http://0x7f.1", "http://1.2.3.4.5",
  "http://[::1]", "http://[::1]:80/x", "https://x.y:99999", "https://x.y:80", "https://x.y:", "https://x.y:x",
  "https://user:pass@x.y", "https://user@x.y", "https://@x.y", "https://user@/x", "https://x.y/path?q=1&r=2#frag",
  "https://x.y#", "https://x.y?", "https://x.y/~user", "https://x.y/%20", "https://x.y/a%2Eb", "https://ü.com",
  "https://xn--tda.com", "https://x.com😂", "https://x.com👨‍👩‍👧", "https://x.com❤️", "https://x.com/😂",
  "https://x.y\u200b", "https://x.y\u00a0z", "https://a.b`c`", "https://a.b<c", "https://a.b>c", "https://a.b\"c",
  "https://a.b\tc", "https://a.b\\c", "https://a.b|c", "https://a.b^c", "https://a^b.c", "https://a_b.c",
  "https://-a.c", "https://a.b/" + "x".repeat(2050), "https://a.b/" + "x".repeat(2036), "https://a.b/" + "x".repeat(2035) + ")))",
  "https://a.b/" + "é".repeat(2030), "https://a.b/" + "😂".repeat(1018), "https://a.b/" + "😂".repeat(1019), "https://a.b/" + "x".repeat(2037), "https://x.y https://x.y", "https://a.b https://c.d https://e.f https://g.h",
  "https://a.b https://a.b https://c.d https://e.f https://g.h", "**https://a.b** https://c.d", "https://x.y/a.gif",
  "https://x.y/a.GIF?x=1", "https://x.y/a.png", "https://x.y/a.jpg", "https://x.y/a.jpeg", "https://x.y/a.webp",
  "https://x.y/a.avif", "https://x.y/a.bmp", "https://x.y/a.gif/", "https://x.y/a.gif#x", "https://x.y/a%2Egif",
  "https://x.y/a.gi%66", "https://media.klipy.com/x/y.gif", "https://media.giphy.com/media/abc/giphy.gif?cid=123",
  "  https://x.y/a.gif  ", "https://x.y/a.gif\n", "<https://x.y/a.gif>", "https://x.y/a.gif https://x.y/b.gif",
  "**https://x.y/a.gif**", "look https://x.y/a.gif", "https://x.y/a.gif\u200b", "\u3000https://x.y/a.gif",
  "https://x.y/a.gif\ufeff", "https://x.y/a.gif\u0085", "`https://x.y`https://z.w", "https://x.y`code`",
  "https://x.y$5$", "https://x.y*a*", "https://x.y_a_", "https://a.b/c_(d)_e", "https://a.b/(c", "https://a.b/c)",
  "https://a.b/((c))", "https://a.b/(c))", "https://a.b/[c]]", "https://a.b/{c}}", "https://a.b/c...", "https://a.b/c?!",
  "https://a.b/c'.", "https://a.b/c\".", "http://a.b.", "http://a..b", "http://.a.b", "http://a.b:8080.",
  "https://x.y\nhttps://z.w", "https://x.y\r\nhttps://z.w", "https://x.y,https://z.w", "https://x.y;https://z.w",
  "decibell://invite/ABCD1234", "decibell://invite/ABCD1234 decibell://invite/ABCD1234", "DECIBELL://INVITE/ABCD",
  "(decibell://invite/ABCD1234)", "decibell://invite/ABCD1234.", "decibell://invite/abc", "decibell://invite/",
  "decibell://invites/ABCD", "xdecibell://invite/ABCD", "decibell://invite/host:1234/ABCD1234",
  "**decibell://invite/ABCD1234**", "<decibell://invite/host:1/ABCD>", "decibell://invite/ABCD https://x.y",
);

// ── emoji ──
add("emoji",
  "😂", "😂😂", "😂😂😂", "😂😂😂😂", "😂😂😂😂😂", "😂 😂", " 😂 ", "😂\n😂", "😂a", "a😂", "❤", "❤️", "❤︎",
  "©", "©️", "™", "™️", "‼️", "#️⃣", "#⃣", "1️⃣", "1⃣", "*️⃣", "*⃣", "#", "1", "👍🏽", "👍🏻👍🏿", "🏽", "🏽🏽",
  "👨‍👩‍👧‍👦", "👨‍👩‍👧", "👩‍❤️‍👨", "👩‍❤‍👨", "👩‍❤️‍💋‍👨", "🏳️‍🌈", "🏳‍🌈", "🏴‍☠️", "🇺🇸", "🇺🇸🇬🇧", "🇺",
  "🇺🇺", "🇦🇺🇸", "🏴󠁧󠁢󠁥󠁮󠁧󠁿", "🏴󠁧󠁢", "😶‍🌫️", "❤️‍🔥", "🧑🏽‍💻", "👁️‍🗨️", "🫩", "🫠", "☺", "☺️", "✌", "✌️",
  "✌🏽", "☝", "☝️", "⛓️‍💥", "🙂‍↔️", "🚶‍➡️", "🧑‍🧑‍🧒", "\u200d", "a\u200db", "\ufe0f", "x\ufe0f", "😂\ufe0f",
  "😂\ufe0e", "↔", "↔️", "▶", "▶️", "♀", "♀️", "⚕", "Ⓜ", "〰", "〽", "㊗", "🅰", "🅰️", "🈂", "🈷️", "🀄", "🃏",
  "👨🏽‍🦰", "🧔‍♂️", "🧔🏻‍♀️", "👮‍♀️", "👮🏿‍♂️", "🤝🏽", "🫱🏻‍🫲🏿", "👭🏼", "🧑🏻‍🤝‍🧑🏿", "🐈‍⬛", "🐦‍🔥", "🍋‍🟩",
  "⚧", "⚧️", "🏳️‍⚧️", "♾", "♾️", "🔟", "🆗", "🇪🇺", "🇺🇳", "🇦", "🇿🇿", "🏻‍", "👍‍", "‍👍", "😂‍😂", "👨\u200d",
  "😂 is funny", "funny 😂", "😂 😂\n😂", "😂\t😂", "😂\u3000😂", "😂\u200b", "😂\ufeff", "😂\u0085", "😂\u00a0",
  "😂*", "\\*😂", "**😂**", "😂 https://x.y", "😂`a`", "_😂_", "\u2028😂", "😂\u2029",
);

// ── shortcodes ──
add("shortcodes",
  ":smile:", ":Smile:", ":SMILE:", ":+1:", ":-1:", ":100:", ":1234:", ":flag-us:", ":flags:", ":nope:", "::smile:",
  ":smile::smile:", ":smile:smile:", "a:smile:b", ":smile", "smile:", ":heart:", ":heart::fire:", "`:smile:`",
  "**:smile:**", ":smile_cat:", ":a:smile:", "🇦:flag-us:", ":flag-us:🇸", ":+1:🏽", "👨‍:woman:", "👨‍👩‍:girl:",
  ":skin-tone-2:", ":smile: :smile:", ":smile:\n:smile:", " :smile: ", ":smile:x", "x:smile:", ":sm ile:",
  ":smi*le:", ":smile:*", "*:smile:*", "_:smile:_", ":smile:_", ":_smile_:", ":__:", ":+:", ":-:", "::", ":::",
  ":a::b::c:", ":grinning::smiley::smile::grin:", ":hash:", ":keycap_star:", ":zero:", ":one:", ":copyright:",
  ":registered:", ":tm:", ":heart_on_fire:", ":face_in_clouds:", ":rainbow-flag:", ":pirate_flag:",
  ":man-woman-girl-boy:", ":eye-in-speech-bubble:", ":shrug:", ":point_up:", ":v:", ":relaxed:", ":black_cat:",
  ":e-mail:", ":email:", ":sweat_smile:", ":rolling_on_the_floor_laughing:", ":joy:", ":JOY:", ":Joy:",
  ":smile:\ufe0f", ":smile:🏽", "\u200d:smile:", "#:zwj:", "1\ufe0f:", ":heart:\ufe0f", ":heart:\u200d🔥",
  "[:smile:](x)", "<:smile:>", "$:smile:$", "https://x.y/:smile:", ":smile:https://x.y",
);

// ── whitespace, line breaks, CRLF, tabs, RTL, combining ──
add("text",
  "", " ", "  ", "\n", "\n\n", "\t", "a", "a\nb", "a\r\nb", "a\rb", "a\n\nb", "a\n\n\nb", "\na", "a\n", "a\tb",
  "\u2028", "\u2029", "a\u2028b", "a\u0085b", "a\u00a0b", "a\u200bb", "a\ufeffb", "\ufeff", "שלום עולם", "مرحبا",
  "a\u0301", "\u0301", "‮abc", "\u200f*a*", "\u202a", "a\u000bb", "a\u000cb", "\u0000", "a\u0000b", "\u007f",
  "  a  ", "\t:smile:\n", "**a**\n`b`", "a\n\n**b**\n\nc", "x\n```y```\nz", "Hello world", "hello, world!",
  "a  b", "a\u3000b", "a\u1680b", "a\u2000b\u200ab", "a\u202fb", "a\u205fb", "ǅ", "İstanbul", "ıi", "ß", "ﬁ",
  "𝔘𝔫𝔦𝔠𝔬𝔡𝔢", "a\u0300\u0301\u0302", "e\u0301*x*", "Ελληνικά", "日本語", "한국어", "ไทย", "\ud83d\ude00".slice(0, 2),
  "**a**\r\n**b**", "a\r", "\r", "\r\n", "\r\n\r\n", "x\r\n```js\r\ny\r\n```\r\nz", "$x$\r\n$$y$$\r\n",
);

// ── composer-specific ──
add("composer",
  "```", "```a```", "``` ``` ```", "a```b", "\\```", "``````", "```js\nx", "```js\nx\n```\n```", "``` `` ```",
  "😂:smile:", ":smile😂:", ":smile:😂:smile:", ":heart:❤️", "❤️:heart:", ":+1::+1:", "::+1::", ":x:smile:",
);

// ── URLs (record also has parseInviteLink / isInviteLink / mediaLabelFor / isGifUrl of the input) ──
const urlCases = [
  "decibell://invite/ABCD1234", "decibell://invite/abcd", "decibell://invite/abc", "decibell://invite/" + "a".repeat(64),
  "decibell://invite/" + "a".repeat(65), "decibell://invite/host:1234/ABCD1234", "decibell://invite/host/1234/ABCD1234",
  "decibell://invite/[::1]:8080/ABCD1234", "decibell://invite/[::1]/8080/ABCD", "decibell://invite/host:0/ABCD",
  "decibell://invite/host:65535/ABCD", "decibell://invite/host:65536/ABCD", "decibell://invite/host:123456/ABCD",
  "decibell://invite/ABCD1234/", "decibell://invite/ABCD1234//", "DECIBELL://INVITE/ABCD", "Decibell://Invite/AbCd",
  "decibell://invite/ABCD1234.", "decibell://invite/AB-CD", "decibell://invite/ABCD1234?x", "decibell://invite/[]:80/ABCD",
  "decibell://invite/[zz]:80/ABCD", "decibell://invite/[ab]:80/ABCD", "decibell://invite/[AB:cd]:80/ABCD",
  "decibell://invite/a:b:80/ABCD", "decibell://invite/ü:80/ABCD", "decibell://invites/ABCD", "decibell:/invite/ABCD",
  "xdecibell://invite/ABCD", "decibell://invite/h:00080/ABCD", "decibell://invite/h:080/ABCD", "decibell://invite/h:/ABCD",
  "decibell://invite/:80/ABCD", "decibell://invite//80/ABCD", "decibell://invite/h/0/ABCD", "decibell://invite/h/99999/ABCD",
  "decibell://invite/h/1/ABCD/", "decibell://invite/h:1/ABCD/x", "decibell://invite/h h:1/ABCD", "decibell://invite/h\u00a0:1/ABCD",
  "decibell://invite/ABCD\n", "decibell://invite/ABCD ", " decibell://invite/ABCD", "decibell://invite/ABCD\u0130",
  "decibell://invite/ABCD\u017f", "decibell://invite/ABC\u212a", "decibell://invite/[1.2.3.4]:5/ABCD",
  "decibell://invite/[::]:5/ABCD", "decibell://invite/[:]:5/ABCD", "decibell://invite/[.]:5/ABCD", "decibell://invite/[::1]x:5/ABCD",
  "decibell://invite/x[::1]:5/ABCD", "decibell://invite/[::1]]:5/ABCD", "decibell://invite/[[::1]:5/ABCD",
  "decibell://invite/host:1234/" + "A".repeat(64), "decibell://invite/host:1234/" + "A".repeat(65), "decibell://invite/1234/5678/abcd",
  "decibell://invite/abcd/", "decibell://invite/1234/abcd", "decibell://invite/h:1/2/abcd", "decibell://invite/h/1:2/abcd",
  "https://x.y/a.gif", "https://x.y/a.GIF", "https://x.y/a.gif?x", "https://x.y/a.gif#y", "https://x.y/a.gif/", "https://x.y/a.gif/.",
  "https://x.y/a.gif/..", "https://x.y/b/../a.gif", "https://x.y/a.gif/%2e", "https://x.y/a.gif/%2E%2e", "https://x.y/a%2egif",
  "https://x.y/a%2Egif", "https://x.y/a.gi%66", "https://x.y/a.%67if", "https://x.y/a.gif%20", "https://x.y/a.gif\u0001",
  "https://x.y/a.gif ", " https://x.y/a.gif", "https://x.y/a.gif\t", "https://x.y/a.g\tif", "https://x.y/a.g\nif",
  "https://x.y\\a.gif", "https://x.y/a.gif\\", "https://x.y/a.gif;x", "https://x.y/a.gif%3F", "https://x.y/a.gif%23",
  "https://x.y/a.png", "https://x.y/a.PNG", "https://x.y/a.jpg", "https://x.y/a.jpeg", "https://x.y/a.jpe", "https://x.y/a.JPEG",
  "https://x.y/a.webp", "https://x.y/a.avif", "https://x.y/a.svg", "https://x.y/a.bmp", "https://x.y/.gif", "https://x.y/gif",
  "https://x.y/a.gifx", "https://x.y/a.gif.png", "https://x.y/a.png.gif", "https://x.y/a.gıf", "https://x.y/a.ğif",
  "https://x.y/ü.gif", "https://x.y/a b.gif", "https://x.y/a\"b.gif", "https://x.y/a<b.gif", "https://x.y/a{b}.gif",
  "https://x.y/a|b.gif", "https://x.y/a^b.gif", "https://x.y/a`b.gif", "https://x.y/%zz.gif", "https://x.y/%.gif",
  "https://x.y/a.gif%", "https://x.y/a.gif%2", "http://x.y:80/a.gif", "http://x.y:99999/a.gif", "http://[::1]/a.gif",
  "http://[::1/a.gif", "http://x.y:x/a.gif", "http://x/a.gif", "http:/x.y/a.gif", "http:x.y/a.gif", "HTTP://X.Y/A.GIF",
  "ftp://x.y/a.gif", "x.y/a.gif", "/a.gif", "", "a.gif", "https://x.y", "https://x.y/", "https://x.y?a.gif", "https://x.y#a.gif",
  "https://user:pw@x.y/a.gif", "https://x.y/a.gif@z", "https://ü.com/a.gif", "https://x.com😂/a.gif", "https://a\u200db.com/a.gif",
  "https://a%20b.com/a.gif", "https://a%zz.com/a.gif", "https://a%ff.com/a.gif", "https://%2e/a.gif", "https://0x7f.1/a.gif",
  "https://1.2.3.4.5/a.gif", "https://a.b.09/a.gif", "https://a.b.0x/a.gif", "https://a.b.0x0x/a.gif", "https:///x.y/a.gif",
  "https:\\\\x.y\\a.gif", "https://x.y/a.gif\u0000", "https://x.y/%41.gif", "https://x.y/a.%47IF",
];

export const cases = Object.entries(groups).flatMap(([g, xs]) => xs.map((x) => ({ group: g, input: x })));
export { urlCases };
