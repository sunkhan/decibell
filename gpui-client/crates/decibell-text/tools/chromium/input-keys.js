// What Chromium's editing does with real key events, for decibell_text::input's vectors
// (../README.md, "Input vectors").
//
// A hidden Electron window with a <textarea> and an <input>; keys are sent with
// webContents.sendInputEvent (Blink's own key handling, not synthetic DOM events), and the
// value and selection are read back after each. Writes one JSON file:
//
//   electron input-keys.js --out chromium-input.json
//
// Nothing here touches the Decibell app, its profile, the network or the clipboard: run it
// with a throwaway --user-data-dir (input-keys.sh).

const { app, BrowserWindow } = require("electron");
const fs = require("fs");

const out = process.argv[process.argv.indexOf("--out") + 1];
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

const WORD_STRINGS = [
  "hello, wörld_1  foo.bar",
  "one  two\tthree",
  "a ... b",
  "foo-bar baz/qux",
  "don't stop",
  "x=1+2*3",
  "ışık ığdır İstanbul",
  "日本語のテキスト",
  "emoji 😀 mid 👨‍👩‍👧 end",
  "  leading and trailing  ",
  "line one\nline two\n\nfour",
  "http://example.com/a?b=c",
  "under_score CamelCase",
  "3.14 1,000 v2.0",
  "(parens) [brackets] {braces}",
  "tab\tand  spaces",
];
const GRAPHEME_STRINGS = [
  "a👨‍👩‍👧‍👦b",
  "🇹🇷🇩🇪",
  "1️⃣2",
  "👍🏽x",
  "éä́",
  "ığİ",
  "한글กำ",
  "a\r\nb",
];

app.commandLine.appendSwitch("disable-renderer-backgrounding");
app.commandLine.appendSwitch("disable-background-timer-throttling");

app.whenReady().then(async () => {
  const win = new BrowserWindow({ width: 600, height: 400, show: false, webPreferences: { backgroundThrottling: false } });
  await win.loadURL(
    "data:text/html;charset=utf-8," +
      encodeURIComponent('<textarea id="area" rows="8" cols="60"></textarea><input id="line" size="60">'),
  );
  const contents = win.webContents;
  contents.focus();
  const js = (code) => contents.executeJavaScript(code);
  // Input events travel to the page on their own channel: wait until the page has seen
  // the key go up (or the characters arrive) before reading anything back.
  await js(`window.ups = 0; window.inputs = 0;
            addEventListener("keyup", () => window.ups++, true);
            addEventListener("input", () => window.inputs++, true);`);
  const until = async (counter, target) => {
    for (let tries = 0; tries < 400; tries++) {
      if ((await js(`window.${counter}`)) >= target) return;
      await sleep(2);
    }
    throw new Error(`the page did not see the ${counter}`);
  };
  const key = async (keyCode, modifiers = []) => {
    const target = (await js("window.ups")) + 1;
    contents.sendInputEvent({ type: "keyDown", keyCode, modifiers });
    contents.sendInputEvent({ type: "keyUp", keyCode, modifiers });
    await until("ups", target);
  };
  const type = async (text) => {
    for (const character of text) {
      const target = (await js("window.inputs")) + 1;
      contents.sendInputEvent({ type: "char", keyCode: character });
      await until("inputs", target);
    }
  };
  const set = (id, value, at) =>
    js(`(() => { const e = document.getElementById(${JSON.stringify(id)}); e.focus(); e.value = ${JSON.stringify(value)};
         e.setSelectionRange(${at}, ${at}); })()`);
  const state = (id) =>
    js(`(() => { const e = document.getElementById(${JSON.stringify(id)});
         return { value: e.value, start: e.selectionStart, end: e.selectionEnd, direction: e.selectionDirection }; })()`);

  // Walks with a key from one end until the caret stops moving; returns every stop (UTF-16).
  const walk = async (id, text, from, keyCode, modifiers) => {
    await set(id, text, from);
    const stops = [from];
    for (let step = 0; step < 200; step++) {
      await key(keyCode, modifiers);
      const now = (await state(id)).start;
      if (now === stops[stops.length - 1]) break;
      stops.push(now);
    }
    return stops;
  };
  // Deletes with a key until nothing is left to delete; returns the value after each.
  const erase = async (id, text, from, keyCode, modifiers) => {
    await set(id, text, from);
    const values = [];
    for (let step = 0; step < 200; step++) {
      const before = (await state(id)).value;
      await key(keyCode, modifiers);
      const after = (await state(id)).value;
      if (after === before) break;
      values.push(after);
    }
    return values;
  };

  const result = { chromium: process.versions.chrome, platform: process.platform, words: [], graphemes: [], undo: [], misc: {} };

  for (const text of WORD_STRINGS) {
    result.words.push({
      text,
      right: await walk("area", text, 0, "Right", ["control"]),
      left: await walk("area", text, text.length, "Left", ["control"]),
      delete: await erase("area", text, 0, "Delete", ["control"]),
      backspace: await erase("area", text, text.length, "Backspace", ["control"]),
    });
  }
  for (const text of GRAPHEME_STRINGS) {
    // A textarea normalises \r\n to \n: read the value back.
    await set("area", text, 0);
    const value = (await state("area")).value;
    result.graphemes.push({
      text: value,
      right: await walk("area", value, 0, "Right", []),
      left: await walk("area", value, value.length, "Left", []),
      backspace: await erase("area", value, value.length, "Backspace", []),
      delete: await erase("area", value, 0, "Delete", []),
    });
  }

  // Undo grouping: each case is a list of steps; after them Ctrl+Z is pressed until the
  // value stops changing, and every value on the way is recorded.
  const UNDO = {
    burst: [["type", "hello"]],
    burst_with_pause: [["type", "abc"], ["wait", 1500], ["type", "def"]],
    burst_with_long_pause: [["type", "abc"], ["wait", 4000], ["type", "def"]],
    backspace_in_burst: [["type", "abcd"], ["key", "Backspace"], ["type", "e"]],
    delete_after_typing: [["type", "abcd"], ["key", "Left"], ["key", "Delete"]],
    caret_move: [["type", "abc"], ["key", "Left"], ["key", "Right"], ["type", "d"]],
    words: [["type", "one two three"]],
    enter: [["type", "one"], ["key", "Enter"], ["type", "two"]],
    insert_text_command: [["type", "abc"], ["insert", " PASTED"], ["type", "!"]],
    select_all_retype: [["type", "abc"], ["key", "a", ["control"]], ["type", "x"]],
    word_delete: [["type", "one two"], ["key", "Backspace", ["control"]], ["type", "x"]],
  };
  for (const [name, steps] of Object.entries(UNDO)) {
    await js(`document.getElementById("area").remove(); document.body.insertAdjacentHTML("afterbegin", '<textarea id="area" rows="8" cols="60"></textarea>'); document.getElementById("area").focus()`);
    for (const [kind, value, modifiers] of steps) {
      if (kind === "type") await type(value);
      else if (kind === "wait") await sleep(value);
      else if (kind === "key") await key(value, modifiers || []);
      else if (kind === "insert") await js(`document.execCommand("insertText", false, ${JSON.stringify(value)})`);
    }
    const typed = (await state("area")).value;
    const values = [];
    for (let step = 0; step < 20; step++) {
      await key("z", ["control"]);
      const now = (await state("area")).value;
      if (values.length ? now === values[values.length - 1] : now === typed) break;
      values.push(now);
    }
    const redone = [];
    for (let step = 0; step < 20; step++) {
      await key("z", ["control", "shift"]);
      const now = (await state("area")).value;
      if (redone.length ? now === redone[redone.length - 1] : now === values[values.length - 1]) break;
      redone.push(now);
    }
    result.undo.push({ name, steps, typed, undo: values, redo: redone });
  }

  // Odds and ends.
  const selection = async (id, text, from, to, keyCode, modifiers) => {
    await js(`(() => { const e = document.getElementById(${JSON.stringify(id)}); e.focus(); e.value = ${JSON.stringify(text)};
         e.setSelectionRange(${Math.min(from, to)}, ${Math.max(from, to)}, ${JSON.stringify(from <= to ? "forward" : "backward")}); })()`);
    await key(keyCode, modifiers);
    const now = await state(id);
    return [now.start, now.end, now.direction];
  };
  result.misc.left_collapses = await selection("area", "abcdef", 2, 4, "Left", []);
  result.misc.right_collapses = await selection("area", "abcdef", 2, 4, "Right", []);
  result.misc.left_collapses_backward = await selection("area", "abcdef", 4, 2, "Left", []);
  result.misc.word_left_from_selection = await selection("area", "one two three", 4, 10, "Left", ["control"]);
  result.misc.word_right_from_selection = await selection("area", "one two three", 4, 10, "Right", ["control"]);
  result.misc.word_right_from_backward_selection = await selection("area", "one two three", 10, 4, "Right", ["control"]);
  result.misc.shift_word_right = await selection("area", "one two three", 0, 0, "Right", ["control", "shift"]);
  result.misc.input_up = await selection("line", "abcdef", 3, 3, "Up", []);
  result.misc.input_down = await selection("line", "abcdef", 3, 3, "Down", []);
  result.misc.area_up_first_line = await selection("area", "abcdef\nxyz", 3, 3, "Up", []);
  result.misc.area_down_last_line = await selection("area", "abcdef\nxyz", 8, 8, "Down", []);
  result.misc.home_end = [await selection("area", "ab\ncdef\ngh", 5, 5, "Home", []), await selection("area", "ab\ncdef\ngh", 5, 5, "End", [])];
  result.misc.ctrl_home_end = [await selection("area", "ab\ncdef\ngh", 5, 5, "Home", ["control"]), await selection("area", "ab\ncdef\ngh", 5, 5, "End", ["control"])];
  // Goal column: down from a long line through a short one to a long one.
  await set("area", "0123456789\nab\n0123456789", 8);
  const column = [];
  for (const keyCode of ["Down", "Down", "Up", "Up"]) {
    await key(keyCode);
    column.push((await state("area")).start);
  }
  result.misc.goal_column = column;
  // A multi-line insert into a single line; maxlength.
  await set("line", "", 0);
  await js(`document.execCommand("insertText", false, "one\\r\\ntwo\\nthree\\rfour")`);
  result.misc.input_newlines = (await state("line")).value;
  await js(`document.getElementById("line").maxLength = 6`);
  await set("line", "abcd", 2);
  await js(`document.execCommand("insertText", false, "😀😀x")`);
  result.misc.maxlength_insert = (await state("line")).value;
  await set("line", "", 0);
  await js(`document.execCommand("insertText", false, "a😀😀")`);
  await js(`document.getElementById("line").maxLength = 3`);
  await set("line", "", 0);
  await js(`document.execCommand("insertText", false, "a😀😀")`);
  result.misc.maxlength_odd = (await state("line")).value;
  // The caret's blink interval as Blink reports it to the page is not exposed; the default
  // (LayoutTheme::CaretBlinkInterval) is 500 ms.

  fs.writeFileSync(out, JSON.stringify(result, null, 1));
  app.quit();
});
