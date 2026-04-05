export default function AboutTab() {
  return (
    <div>
      {/* App info card */}
      <div className="rounded-xl bg-bg-primary px-5 py-5">
        <div className="flex items-center gap-3 mb-3">
          <div className="flex h-10 w-10 items-center justify-center rounded-xl bg-gradient-to-br from-accent to-accent-bright">
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="white" strokeWidth="2.5" strokeLinecap="round">
              <path d="M9 18V5l12-2v13" />
              <circle cx="6" cy="18" r="3" fill="white" stroke="none" />
              <circle cx="18" cy="16" r="3" fill="white" stroke="none" />
            </svg>
          </div>
          <div>
            <div className="text-sm font-extrabold text-text-bright">Decibell</div>
            <div className="text-[11px] text-text-muted">Decentralized game chat</div>
          </div>
        </div>
        <div className="text-[12px] text-text-secondary">
          Version <span className="font-semibold text-text-primary">0.3.2</span>
        </div>
      </div>
    </div>
  );
}
