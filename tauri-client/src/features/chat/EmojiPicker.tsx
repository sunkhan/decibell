import { useEffect, useMemo, useRef, useState } from "react";
import data from "@emoji-mart/data";
import Twemoji from "../../components/emoji/Twemoji";

interface EmojiEntry {
  id: string;
  name: string;
  keywords: string[];
  skins: { native: string }[];
}
interface CategoryEntry {
  id: string;
  emojis: string[];
}

const CATEGORIES = (data as any).categories as CategoryEntry[];
const EMOJIS = (data as any).emojis as Record<string, EmojiEntry>;

const CATEGORY_LABELS: Record<string, string> = {
  frequent: "Frequently used",
  people: "Smileys & People",
  nature: "Animals & Nature",
  foods: "Food & Drink",
  activity: "Activity",
  places: "Travel & Places",
  objects: "Objects",
  symbols: "Symbols",
  flags: "Flags",
};

const CATEGORY_ICONS: Record<string, string> = {
  frequent: "🕘",
  people: "😀",
  nature: "🐶",
  foods: "🍔",
  activity: "⚽",
  places: "🚗",
  objects: "💡",
  symbols: "❤️",
  flags: "🏁",
};

const RECENT_KEY = "decibell.emoji.recent";
const RECENT_MAX = 24;

function loadRecent(): string[] {
  try {
    const raw = localStorage.getItem(RECENT_KEY);
    if (!raw) return [];
    const parsed = JSON.parse(raw);
    return Array.isArray(parsed) ? parsed.filter((x) => typeof x === "string") : [];
  } catch {
    return [];
  }
}

function saveRecent(ids: string[]) {
  try {
    localStorage.setItem(RECENT_KEY, JSON.stringify(ids));
  } catch {
    // ignore quota errors
  }
}

function nativeOf(id: string): string | null {
  return EMOJIS[id]?.skins?.[0]?.native ?? null;
}

interface EmojiPickerProps {
  onSelect: (emoji: string) => void;
  onClose: () => void;
  triggerRef?: React.RefObject<HTMLElement | null>;
}

export default function EmojiPicker({ onSelect, onClose, triggerRef }: EmojiPickerProps) {
  const containerRef = useRef<HTMLDivElement>(null);
  const scrollRef = useRef<HTMLDivElement>(null);
  const sectionRefs = useRef<Record<string, HTMLDivElement | null>>({});
  const searchRef = useRef<HTMLInputElement>(null);

  const [search, setSearch] = useState("");
  const [recent, setRecent] = useState<string[]>(() => loadRecent());
  const [activeCategory, setActiveCategory] = useState<string>(
    () => (loadRecent().length ? "frequent" : CATEGORIES[0]?.id ?? "people")
  );
  const [hoveredEmoji, setHoveredEmoji] = useState<{ native: string; name: string; id: string } | null>(null);

  // Close on outside click / Escape
  useEffect(() => {
    const handleMouseDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (
        containerRef.current &&
        !containerRef.current.contains(target) &&
        !triggerRef?.current?.contains(target)
      ) {
        onClose();
      }
    };
    const handleEscape = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    const id = window.setTimeout(() => {
      document.addEventListener("mousedown", handleMouseDown);
      document.addEventListener("keydown", handleEscape);
    }, 0);
    return () => {
      window.clearTimeout(id);
      document.removeEventListener("mousedown", handleMouseDown);
      document.removeEventListener("keydown", handleEscape);
    };
  }, [onClose, triggerRef]);

  // Autofocus search on mount
  useEffect(() => {
    searchRef.current?.focus();
  }, []);

  const trimmedSearch = search.trim().toLowerCase();
  const searchResults = useMemo<string[]>(() => {
    if (!trimmedSearch) return [];
    const out: string[] = [];
    for (const [id, emoji] of Object.entries(EMOJIS)) {
      if (
        id.includes(trimmedSearch) ||
        emoji.name.toLowerCase().includes(trimmedSearch) ||
        emoji.keywords.some((k) => k.toLowerCase().includes(trimmedSearch))
      ) {
        out.push(id);
        if (out.length >= 120) break;
      }
    }
    return out;
  }, [trimmedSearch]);

  const handlePick = (id: string) => {
    const native = nativeOf(id);
    if (!native) return;
    onSelect(native);
    const next = [id, ...recent.filter((x) => x !== id)].slice(0, RECENT_MAX);
    setRecent(next);
    saveRecent(next);
  };

  // Observe which category section is currently in view
  useEffect(() => {
    const scroller = scrollRef.current;
    if (!scroller || trimmedSearch) return;
    const observer = new IntersectionObserver(
      (entries) => {
        for (const e of entries) {
          if (e.isIntersecting) {
            const id = (e.target as HTMLElement).dataset.categoryId;
            if (id) setActiveCategory(id);
          }
        }
      },
      { root: scroller, rootMargin: "-10% 0px -80% 0px", threshold: 0 }
    );
    for (const el of Object.values(sectionRefs.current)) {
      if (el) observer.observe(el);
    }
    return () => observer.disconnect();
  }, [trimmedSearch, recent.length]);

  const scrollToCategory = (id: string) => {
    const el = sectionRefs.current[id];
    if (el && scrollRef.current) {
      scrollRef.current.scrollTo({ top: el.offsetTop - 4, behavior: "smooth" });
    }
  };

  const navCategories = useMemo(() => {
    const base = CATEGORIES.map((c) => c.id);
    return recent.length > 0 ? ["frequent", ...base] : base;
  }, [recent.length]);

  return (
    <div
      ref={containerRef}
      className="absolute bottom-full right-0 z-50 mb-2 flex h-[420px] w-[352px] animate-[pickerIn_0.2s_ease] flex-col overflow-hidden rounded-[14px] border border-border bg-bg-light shadow-[0_12px_48px_rgba(0,0,0,0.5),0_0_0_1px_rgba(255,255,255,0.02)]"
    >
      {/* Search */}
      <div className="shrink-0 px-3 pb-2 pt-3">
        <div className="flex items-center gap-2 rounded-[10px] border border-border bg-bg-lighter px-3 transition-all focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--color-accent-soft)]"
          style={{ height: 36 }}
        >
          <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" className="shrink-0 text-text-muted">
            <circle cx="11" cy="11" r="8" />
            <line x1="21" y1="21" x2="16.65" y2="16.65" />
          </svg>
          <input
            ref={searchRef}
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="Search emoji..."
            className="flex-1 bg-transparent text-[13px] text-text-primary outline-none placeholder:text-text-faint"
          />
          {search && (
            <button
              onClick={() => setSearch("")}
              className="text-text-muted transition-colors hover:text-text-secondary"
              title="Clear"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          )}
        </div>
      </div>

      {/* Scrollable body */}
      <div
        ref={scrollRef}
        className="flex-1 overflow-y-auto px-2 pb-1 scrollbar-thin"
      >
        {trimmedSearch ? (
          searchResults.length === 0 ? (
            <div className="flex flex-col items-center justify-center gap-2.5 py-12 text-center text-text-muted">
              <div className="flex h-10 w-10 items-center justify-center rounded-full bg-text-muted/10">
                <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                  <circle cx="11" cy="11" r="8" />
                  <line x1="21" y1="21" x2="16.65" y2="16.65" />
                </svg>
              </div>
              <span className="text-[12px]">
                No emoji match "{search}"
              </span>
            </div>
          ) : (
            <EmojiGrid ids={searchResults} onPick={handlePick} onHover={setHoveredEmoji} />
          )
        ) : (
          <>
            {recent.length > 0 && (
              <Section
                id="frequent"
                label={CATEGORY_LABELS.frequent}
                sectionRef={(el) => (sectionRefs.current.frequent = el)}
              >
                <EmojiGrid
                  ids={recent.filter((id) => EMOJIS[id])}
                  onPick={handlePick}
                  onHover={setHoveredEmoji}
                />
              </Section>
            )}
            {CATEGORIES.map((cat) => (
              <Section
                key={cat.id}
                id={cat.id}
                label={CATEGORY_LABELS[cat.id] ?? cat.id}
                sectionRef={(el) => (sectionRefs.current[cat.id] = el)}
              >
                <EmojiGrid ids={cat.emojis} onPick={handlePick} onHover={setHoveredEmoji} />
              </Section>
            ))}
          </>
        )}
      </div>

      {/* Category nav */}
      <div className="flex shrink-0 items-center gap-[2px] border-t border-border-divider bg-bg-mid px-1.5 py-1">
        {navCategories.map((id) => {
          const isActive = activeCategory === id && !trimmedSearch;
          return (
            <button
              key={id}
              onClick={() => scrollToCategory(id)}
              title={CATEGORY_LABELS[id] ?? id}
              className={`relative flex h-[34px] flex-1 cursor-pointer items-center justify-center rounded-lg transition-colors ${
                isActive
                  ? "bg-accent-soft"
                  : "hover:bg-surface-hover"
              }`}
            >
              <Twemoji emoji={CATEGORY_ICONS[id] ?? "❓"} size={18} />
              {isActive && (
                <span className="absolute bottom-[2px] left-1/2 h-[2px] w-4 -translate-x-1/2 rounded-full bg-accent" />
              )}
            </button>
          );
        })}
      </div>

      {/* Preview bar */}
      <div className="flex h-[46px] shrink-0 items-center gap-2.5 border-t border-border-divider px-3.5">
        {hoveredEmoji ? (
          <>
            <Twemoji emoji={hoveredEmoji.native} size={28} />
            <span className="font-mono text-[12px] font-medium text-text-secondary">
              :{hoveredEmoji.id}:
            </span>
          </>
        ) : (
          <span className="text-[12px] text-text-faint">Hover to preview</span>
        )}
      </div>
    </div>
  );
}

function Section({
  id,
  label,
  sectionRef,
  children,
}: {
  id: string;
  label: string;
  sectionRef: (el: HTMLDivElement | null) => void;
  children: React.ReactNode;
}) {
  return (
    <div
      ref={sectionRef}
      data-category-id={id}
      className="mb-1"
    >
      <div className="sticky top-0 z-10 bg-bg-light px-1.5 py-[6px] text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
        {label}
      </div>
      {children}
    </div>
  );
}

function EmojiGrid({
  ids,
  onPick,
  onHover,
}: {
  ids: string[];
  onPick: (id: string) => void;
  onHover: (info: { native: string; name: string; id: string } | null) => void;
}) {
  return (
    <div className="grid grid-cols-8 gap-[2px]">
      {ids.map((id) => {
        const emoji = EMOJIS[id];
        if (!emoji) return null;
        const native = emoji.skins?.[0]?.native;
        if (!native) return null;
        return (
          <button
            key={id}
            onClick={() => onPick(id)}
            onMouseEnter={() => onHover({ native, name: emoji.name, id: emoji.id })}
            onMouseLeave={() => onHover(null)}
            title={emoji.name}
            className="flex h-[38px] w-[38px] cursor-pointer items-center justify-center rounded-lg transition-all hover:scale-110 hover:bg-surface-active active:scale-95"
          >
            <Twemoji emoji={native} size={22} />
          </button>
        );
      })}
    </div>
  );
}
