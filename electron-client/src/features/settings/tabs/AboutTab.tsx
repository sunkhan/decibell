import { useUpdateStore } from "../../../stores/updateStore";

export default function AboutTab() {
  const status = useUpdateStore((s) => s.status);
  const mode = useUpdateStore((s) => s.mode);
  const currentVersion = useUpdateStore((s) => s.currentVersion);

  return (
    <div className="flex flex-col gap-4">
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
          Version <span className="font-medium text-text-primary">{currentVersion || "0.6.5"}</span>
        </div>
      </div>

      {/* Updates card */}
      <div className="rounded-[10px] border border-border-divider bg-bg-light px-5 py-5">
        <div className="mb-3 text-[13px] font-semibold text-text-primary">Updates</div>
        <UpdateStatusRow status={status} mode={mode} />
      </div>
    </div>
  );
}

function UpdateStatusRow({
  status,
  mode,
}: {
  status: ReturnType<typeof useUpdateStore.getState>["status"];
  mode: ReturnType<typeof useUpdateStore.getState>["mode"];
}) {
  const onCheck = () => {
    window.decibell.update.check().catch((err) => {
      console.error("[update] check failed:", err);
    });
  };
  const onRestart = () => {
    window.decibell.update.quitAndInstall().catch((err) => {
      console.error("[update] quitAndInstall failed:", err);
    });
  };
  const onOpenReleasePage = () => {
    window.decibell.update.openReleasePage().catch((err) => {
      console.error("[update] openReleasePage failed:", err);
    });
  };

  if (mode === "disabled") {
    return (
      <div className="text-[12px] text-text-muted">
        Updates are disabled in development builds.
      </div>
    );
  }

  let line: React.ReactNode = null;
  let button: React.ReactNode = null;

  switch (status.state) {
    case "idle":
    case "not-available":
      line = <span className="text-text-secondary">You're up to date.</span>;
      button = <PrimaryButton onClick={onCheck}>Check now</PrimaryButton>;
      break;
    case "checking":
      line = <span className="text-text-secondary">Checking for updates…</span>;
      button = <PrimaryButton disabled>Check now</PrimaryButton>;
      break;
    case "available":
      if (mode === "notify-only") {
        line = (
          <span className="text-text-primary">
            {status.version} is available.
          </span>
        );
        button = (
          <PrimaryButton onClick={onOpenReleasePage}>
            Open release page
          </PrimaryButton>
        );
      } else {
        line = (
          <span className="text-text-secondary">
            Update available — preparing download…
          </span>
        );
        button = <PrimaryButton disabled>Check now</PrimaryButton>;
      }
      break;
    case "downloading":
      line = (
        <span className="text-text-secondary">
          Downloading {status.version}… {Math.round(status.pct)}%
        </span>
      );
      button = <PrimaryButton disabled>Check now</PrimaryButton>;
      break;
    case "downloaded":
      line = (
        <span className="text-text-primary">
          Update ready: {status.version}
        </span>
      );
      button = <PrimaryButton onClick={onRestart}>Restart</PrimaryButton>;
      break;
    case "error":
      line = (
        <span className="text-error">Couldn't check: {status.message}</span>
      );
      button = <PrimaryButton onClick={onCheck}>Try again</PrimaryButton>;
      break;
  }

  return (
    <div className="flex items-center justify-between gap-3">
      <div className="min-w-0 flex-1 text-[12px]">{line}</div>
      <div className="shrink-0">{button}</div>
    </div>
  );
}

function PrimaryButton({
  children,
  onClick,
  disabled,
}: {
  children: React.ReactNode;
  onClick?: () => void;
  disabled?: boolean;
}) {
  return (
    <button
      onClick={onClick}
      disabled={disabled}
      className="rounded-md bg-accent px-3 py-1.5 text-[12px] font-semibold text-white transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50"
    >
      {children}
    </button>
  );
}
