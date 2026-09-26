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
          flex items-center gap-2 px-3 py-2 rounded-xl text-sm font-medium transition-all
          bg-white dark:bg-[#1e1e1e]
          border border-black/10 dark:border-white/8
          text-slate-800 dark:text-slate-100
          hover:bg-slate-50 dark:hover:bg-[#252525]
          hover:border-black/15 dark:hover:border-white/13
          disabled:opacity-50
          shadow-[0_1px_3px_rgba(0,0,0,0.06)] dark:shadow-[0_1px_4px_rgba(0,0,0,0.4)]
          ${dimmed && !open ? "opacity-60" : ""}
          ${open ? "border-red-400/60 dark:border-red-500/40 ring-2 ring-red-500/10 dark:ring-red-500/10" : ""}
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
          className={`w-3.5 h-3.5 text-slate-400 dark:text-slate-500 transition-transform duration-200 shrink-0 ${open ? "rotate-180" : ""}`}
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
          absolute right-0 z-50 ${openUp ? "bottom-full mb-1.5" : "mt-1.5"} min-w-full max-h-64 overflow-y-auto
          bg-white dark:bg-[#1e1e1e]
          border border-black/10 dark:border-white/8
          rounded-xl
          shadow-[0_8px_24px_rgba(0,0,0,0.12)] dark:shadow-[0_8px_32px_rgba(0,0,0,0.6)]
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
                  w-full flex items-center justify-between gap-2 px-3 py-2.5 text-sm font-medium text-left transition-colors
                  ${
                    isActive
                      ? "bg-red-50 dark:bg-red-500/20 text-red-600 dark:text-red-400"
                      : "text-slate-700 dark:text-slate-200 hover:bg-slate-50 dark:hover:bg-white/5"
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
