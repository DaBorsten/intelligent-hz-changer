/** Compact on/off switch for list rows and cards. */
export function Switch({
  checked,
  onChange,
  title,
  large,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  title?: string;
  /** The app-wide master switch in the header. */
  large?: boolean;
}) {
  return (
    <button
      role="switch"
      aria-checked={checked}
      title={title}
      onClick={(e) => {
        // Rows and canvas tiles are clickable too; the switch is its own action.
        e.stopPropagation();
        onChange(!checked);
      }}
      className={`relative inline-flex items-center rounded-full shrink-0 p-0.5 btn-press ${
        large ? "w-9 h-5" : "w-8 h-4.5"
      } ${checked ? "bg-accent" : "bg-switch-off"}`}
      style={{
        transition:
          "background-color 200ms cubic-bezier(0.23, 1, 0.32, 1), transform 140ms cubic-bezier(0.23, 1, 0.32, 1)",
      }}
    >
      <span
        className={`block bg-white rounded-full ${large ? "w-4 h-4" : "w-3.5 h-3.5"}`}
        style={{
          transform: checked ? `translateX(${large ? 16 : 14}px)` : undefined,
          transition: "transform 220ms cubic-bezier(0.34, 1.56, 0.64, 1)",
        }}
      />
    </button>
  );
}
