// The GPUI shell's `auth-register`: the sign-up form (LoginPage after "Register" is clicked).
import LoginPage from "@app/features/auth/LoginPage";
import { $, frames } from "../runtime/harness";

export default function AuthRegister() {
  return <LoginPage />;
}

export async function setup() {
  const register = [...document.querySelectorAll("button")].find((b) => b.textContent?.trim() === "Register");
  register?.click();
  await frames(2);
  return { mode: $("h1").textContent };
}
