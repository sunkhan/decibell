/** A segmented control — row of buttons where exactly one is selected. */
export default function SegmentedControl<T extends string | number>({
  options,
  value,
  onChange,
  disabled,
}: {
  options: { value: T; label: string }[];
  value: T;
  onChange: (v: T) => void;
  disabled?: boolean;
}) {
  return (
    <div className={`flex rounded-md bg-bg-darkest p-[3px] ${disabled ? "opacity-50" : ""}`}>
      {options.map((opt) => (
        <button
          key={String(opt.value)}
          type="button"
          disabled={disabled}
          onClick={() => onChange(opt.value)}
          className={`flex-1 rounded-sm px-3 py-[7px] text-[11px] font-semibold transition-all ${
            value === opt.value
              ? "bg-accent-mid text-accent-bright shadow-[0_0_6px_color-mix(in_srgb,var(--color-accent)_10%,transparent)]"
              : "text-text-muted hover:text-text-secondary"
          }`}
        >
          {opt.label}
        </button>
      ))}
    </div>
  );
}
