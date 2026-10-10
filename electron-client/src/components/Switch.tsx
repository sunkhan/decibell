/**
 * The on/off switch: a 40 × 22 track with a 16 px knob. The knob is laid
 * out by the track (flex + padding), not absolutely positioned — an
 * absolute knob without an explicit inset lands wherever the button's
 * centred content would, which put Public listing's knob outside its track.
 *
 * Put it inside a <label> to name it by the label's text, or pass `label`
 * when it stands next to a heading.
 */
export default function Switch({
  checked,
  onToggle,
  disabled,
  label,
}: {
  checked: boolean;
  onToggle: () => void;
  disabled?: boolean;
  label?: string;
}) {
  return (
    <button
      type="button"
      role="switch"
      aria-checked={checked}
      aria-label={label}
      disabled={disabled}
      onClick={onToggle}
      className={`inline-flex h-5.5 w-10 shrink-0 items-center rounded-full border p-0.5 transition-all disabled:cursor-not-allowed disabled:opacity-50 ${
        checked
          ? "border-accent bg-accent shadow-[0_0_8px_color-mix(in_srgb,var(--color-accent)_22%,transparent)]"
          : "border-border bg-bg-lighter"
      }`}
    >
      <span
        aria-hidden="true"
        className={`h-4 w-4 rounded-full transition-all ${
          checked ? "translate-x-4.5 bg-on-accent" : "translate-x-0 bg-text-muted"
        }`}
      />
    </button>
  );
}
