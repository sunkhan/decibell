export default function AboutTab() {
  return (
    <div>
      {/* App info card */}
      <div className="rounded-[10px] border border-border-divider bg-bg-light px-5 py-5">
        <div className="mb-4 flex items-center gap-3">
          <div className="flex h-10 w-10 items-center justify-center rounded-[10px] bg-gradient-to-br from-accent to-accent-bright">
            <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="white" strokeWidth="2.5" strokeLinecap="round">
              <path d="M9 18V5l12-2v13" />
              <circle cx="6" cy="18" r="3" fill="white" stroke="none" />
              <circle cx="18" cy="16" r="3" fill="white" stroke="none" />
            </svg>
          </div>
          <div>
            <div className="font-display text-[14px] font-semibold text-text-primary">Decibell</div>
            <div className="text-[12px] text-text-muted">Decentralized game chat</div>
          </div>
        </div>
        <div className="text-[12px] text-text-secondary">
          Version <span className="font-medium text-text-primary">0.5.0</span>
        </div>
      </div>
    </div>
  );
}
