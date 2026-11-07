import { useEffect, useRef, useState } from "react";

export function SegmentedControl({
  options,
  value,
  onChange,
}: {
  options: { value: string; label: string }[];
  value: string;
  onChange: (v: string) => void;
}) {
  const containerRef = useRef<HTMLDivElement>(null);
  const buttonRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const activeIndex = options.findIndex((o) => o.value === value);
  const [dragIndex, setDragIndex] = useState<number | null>(null);
  const isDragging = dragIndex !== null;
  const displayIndex = isDragging ? dragIndex : activeIndex;

  const [pillStyle, setPillStyle] = useState<{ left: number; width: number }>({
    left: 0,
    width: 0,
  });

  useEffect(() => {
    const measure = () => {
      const btn = buttonRefs.current[displayIndex];
      if (btn) {
        setPillStyle({ left: btn.offsetLeft, width: btn.offsetWidth });
      }
    };
    measure();
    // Button widths change once the web font loads (and on relayout), which
    // re-renders nothing — so watch the sizes instead of trusting one measure.
    const obs = new ResizeObserver(measure);
    for (const btn of buttonRefs.current) if (btn) obs.observe(btn);
    let cancelled = false;
    void document.fonts.ready.then(() => {
      if (!cancelled) measure();
    });
    return () => {
      cancelled = true;
      obs.disconnect();
    };
  }, [displayIndex, options]);

  function getIndexFromX(x: number): number {
    const el = containerRef.current;
    if (!el) return activeIndex;
    const rect = el.getBoundingClientRect();
    const segW = rect.width / options.length;
    const idx = Math.floor((x - rect.left) / segW);
    return Math.max(0, Math.min(options.length - 1, idx));
  }

  function onPointerDown(e: React.PointerEvent) {
    (e.currentTarget as HTMLElement).setPointerCapture(e.pointerId);
    setDragIndex(getIndexFromX(e.clientX));
  }

  function onPointerMove(e: React.PointerEvent) {
    if (!isDragging) return;
    setDragIndex(getIndexFromX(e.clientX));
  }

  function onPointerUp(e: React.PointerEvent) {
    if (!isDragging) return;
    const idx = getIndexFromX(e.clientX);
    onChange(options[idx].value);
    setDragIndex(null);
  }

  return (
    <div
      ref={containerRef}
      className="segmented hl select-none cursor-pointer shrink-0"
      style={{ touchAction: "none" }}
      onPointerDown={onPointerDown}
      onPointerMove={onPointerMove}
      onPointerUp={onPointerUp}
    >
      <div
        className="absolute top-0.5 bottom-0.5 rounded-md bg-selected pointer-events-none"
        style={{
          left: pillStyle.left,
          width: pillStyle.width,
          transition: isDragging
            ? "left 80ms cubic-bezier(0.23, 1, 0.32, 1)"
            : "left 200ms cubic-bezier(0.23, 1, 0.32, 1)",
        }}
      />
      {options.map((opt, i) => (
        <button
          key={opt.value}
          ref={(el) => {
            buttonRefs.current[i] = el;
          }}
          onClick={() => !isDragging && onChange(opt.value)}
          className={`relative z-10 flex-1 whitespace-nowrap px-3 py-1.25 rounded-md text-xs font-medium transition-colors duration-150 ${
            displayIndex === i
              ? "text-fg"
              : "text-muted hover:text-fg"
          }`}
        >
          {opt.label}
        </button>
      ))}
    </div>
  );
}
