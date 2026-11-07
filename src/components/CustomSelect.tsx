import { useEffect, useRef, useState } from "react";

export interface SelectOption {
  value: string;
  label: string;
}

export function CustomSelect({
  value,
  options,
  onChange,
  disabled,
  dimmed,
}: {
  value: string;
  options: SelectOption[];
  onChange: (v: string) => void;
  disabled?: boolean;
  // Fades only the trigger; opacity on a parent would bleed into the open list.
  dimmed?: boolean;
}) {
  const [open, setOpen] = useState(false);
  // Flip upward when the list would run off the bottom of the window.
  const [openUp, setOpenUp] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  const selected = options.find((o) => o.value === value);

  useEffect(() => {
    function onDown(e: MouseEvent) {
      if (ref.current && !ref.current.contains(e.target as Node))
        setOpen(false);
    }
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, []);

  return (
    <div ref={ref} className="relative shrink-0 select-none">
      <button
        type="button"
        disabled={disabled}
        onClick={(e) => {
          const rect = e.currentTarget.getBoundingClientRect();
          setOpenUp(window.innerHeight - rect.bottom < 270 && rect.top > 270);
          setOpen((o) => !o);
        }}
        className={`
          hl flex items-center justify-between gap-2 min-w-30 h-8.5 px-2.5 rounded-lg font-mono text-xs transition-colors
          bg-well border border-line-strong text-fg
          hover:bg-chip disabled:opacity-50
          ${dimmed && !open ? "opacity-60" : ""}
          ${open ? "border-accent-line" : ""}
        `}
      >
        <span className="relative">
          <span className="invisible whitespace-nowrap" aria-hidden="true">
            {
              options.reduce(
                (a, b) => (b.label.length > a.label.length ? b : a),
                options[0],
              )?.label
            }
          </span>
          <span className="absolute inset-0 flex items-center whitespace-nowrap">
            {selected?.label ?? value}
          </span>
        </span>
        <svg
          className={`w-3.5 h-3.5 text-muted transition-transform duration-200 shrink-0 ${open ? "rotate-180" : ""}`}
          viewBox="0 0 12 12"
          fill="none"
          xmlns="http://www.w3.org/2000/svg"
        >
          <path
            d="M2 4l4 4 4-4"
            stroke="currentColor"
            strokeWidth="1.5"
            strokeLinecap="round"
            strokeLinejoin="round"
          />
        </svg>
      </button>

      {open && (
        <div
          className={`
          absolute right-0 z-50 ${openUp ? "bottom-full mb-1.5" : "mt-1.5"} min-w-full max-h-64 overflow-y-auto p-1 dropdown-anim
          bg-surface border border-line-strong rounded-lg
          shadow-[0_8px_24px_rgba(0,0,0,0.25)]
        `}
        >
          {options.map((opt) => {
            const isActive = opt.value === value;
            return (
              <button
                key={opt.value}
                type="button"
                onClick={() => {
                  onChange(opt.value);
                  setOpen(false);
                }}
                className={`
                  w-full flex items-center justify-between gap-2 px-2 py-1.5 rounded-md font-mono text-xs text-left transition-colors
                  ${
                    isActive
                      ? "bg-accent-soft text-accent-text"
                      : "text-fg-2 hover:bg-chip"
                  }
                `}
              >
                <span className="whitespace-nowrap">{opt.label}</span>
                {isActive && (
                  <svg
                    className="w-3.5 h-3.5 shrink-0"
                    viewBox="0 0 12 12"
                    fill="none"
                  >
                    <path
                      d="M2 6l3 3 5-5"
                      stroke="currentColor"
                      strokeWidth="1.5"
                      strokeLinecap="round"
                      strokeLinejoin="round"
                    />
                  </svg>
                )}
              </button>
            );
          })}
        </div>
      )}
    </div>
  );
}
