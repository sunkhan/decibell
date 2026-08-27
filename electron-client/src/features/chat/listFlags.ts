// Temporary A/B switch for the Virtuoso → real-DOM message list migration
// (docs/superpowers/specs/2026-08-25-real-dom-message-list-plan.md). Read
// once at module load. Default on; flip off in devtools to compare against
// the Virtuoso path:
//   localStorage.setItem("decibell.real_message_list", "0")
// Deleted together with the Virtuoso paths once parity is signed off.
export const USE_REAL_LIST =
  localStorage.getItem("decibell.real_message_list") !== "0";
