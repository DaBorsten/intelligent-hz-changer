/** Compact on/off switch for list rows and cards. */
export function Switch({
  checked,
  onChange,
  title,
}: {
  checked: boolean;
  onChange: (v: boolean) => void;
  title?: string;
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
      className={`relative inline-flex items-center w-8 h-[18px] rounded-full shrink-0 btn-press ${
        checked ? "bg-red-500" : "bg-slate-300 dark:bg-slate-600"
      }`}
      style={{
        transition:
          "background-color 200ms cubic-bezier(0.23, 1, 0.32, 1), transform 140ms cubic-bezier(0.23, 1, 0.32, 1)",
      }}
    >
      <span
        className="inline-block w-3 h-3 bg-white rounded-full"
        style={{
          transform: checked
            ? "translateX(1.0625rem)"
            : "translateX(0.1875rem)",
          transition: "transform 220ms cubic-bezier(0.34, 1.56, 0.64, 1)",
          boxShadow: "0 1px 3px rgba(0,0,0,0.25)",
        }}
      />
    </button>
  );
}
