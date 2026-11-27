// The GPUI shell's `auth-register-refused`: the sign-up form refused by Chromium's own form
// validation (LoginPage's `required` inputs and `type="email"`). The capture shows the validation
// bubble Blink draws under the first invalid field after a click on Register; `setup()` returns what Chromium
// says for a table of email values (`validationMessage`, `value` after sanitising), both set by
// script and inserted as typed text (`execCommand("insertText")`: the editing path a key press
// takes, which converts an internationalised domain to punycode), and which field the refused
// submit focused. `decibell-shell`'s `validation.rs` is checked against this table
// (`tests::chromium_email_messages`).
import LoginPage from "@app/features/auth/LoginPage";
import { $, centre, click, frames, sleep } from "../runtime/harness";

export default function AuthRegisterRefused() {
  return <LoginPage />;
}

const EMAILS = [
  "", " ", "abc", "abc@", "@abc", "@", "a@b", "a@b.c", "a b@c", "a@b c", "a@b..c", "a@.b", "a@b.", "a@-b",
  "a@b-", "a@b-.c", "a@-b.c", "ä@b", "a@bä.com", "a@é.com", "a@b_c", "a(b)@c", "a@b@c", " a@b ", "a..b@c",
  ".a@b", "a.@b", "a+tag@example.test", "x@123", "a@[1.2.3.4]", "a\"b@c", "a@b,c", "a@b;c", "a@b\\c",
  "a@" + "x".repeat(63) + ".com", "a@" + "x".repeat(64) + ".com", "😀@b", "a@😀.com", "a@b.😀", "a@b:c",
  "a@b..", "a@.", "a@..b", "A@B.COM", "a@b.c.", "a@ b",
];

function insertTyped(input: HTMLInputElement, text: string): void {
  input.focus();
  input.select();
  if (text === "") document.execCommand("delete");
  else document.execCommand("insertText", false, text);
}

export async function setup() {
  const register = [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === "Register");
  register?.click();
  await frames(2);

  const scripted: Record<string, { value: string; message: string }> = {};
  const typed: Record<string, { value: string; message: string }> = {};
  const probe = document.createElement("input");
  probe.type = "email";
  probe.required = true;
  document.body.appendChild(probe);
  for (const email of EMAILS) {
    probe.value = email;
    scripted[email] = { value: probe.value, message: probe.validationMessage };
    insertTyped(probe, email);
    typed[email] = { value: probe.value, message: probe.validationMessage };
  }
  const text = document.createElement("input");
  text.type = "text";
  text.required = true;
  document.body.appendChild(text);
  const emptyText = text.validationMessage;
  text.value = "   ";
  const spacesText = text.validationMessage;
  probe.remove();
  text.remove();

  // The real form: a name, an invalid email, a password, then a click on Register.
  const [name, email, password] = [...document.querySelectorAll<HTMLInputElement>("form input")];
  let submitted = 0;
  document.querySelector("form")!.addEventListener("submit", () => submitted++, true);
  insertTyped(name, "newuser");
  insertTyped(email, "abc");
  insertTyped(password, "hunter22");
  const submit = [...document.querySelectorAll<HTMLButtonElement>("form button")].find((b) => b.type === "submit")!;
  await click(...centre(submit));
  await sleep(300);
  await frames(2);
  const focused = document.activeElement === email ? "email" : document.activeElement?.tagName ?? "none";
  return { scripted, typed, emptyText, spacesText, focused, submitted, emailRect: email.getBoundingClientRect().toJSON() };
}
