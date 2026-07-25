import React from "react";
import { UserPlus, Pencil, Lock } from "lucide-react";

interface WelcomeStateProps {
  channelName?: string;
  onInvite?: () => void;
  onEditTopic?: () => void;
  onSetRules?: () => void;
}

const WelcomeState: React.FC<WelcomeStateProps> = ({
  channelName = "general",
  onInvite,
  onEditTopic,
  onSetRules,
}) => {
  return (
    <>
      <style>{`
        @keyframes welcome-fadeUp {
          from { opacity: 0; transform: translateY(16px); }
          to { opacity: 1; transform: translateY(0); }
        }
        @keyframes welcome-spin-slow {
          to { transform: rotate(360deg); }
        }

        .welcome-state {
          text-align: center;
          max-width: 480px;
          margin: 0 auto;
          padding: 40px 20px;
          animation: welcome-fadeUp 0.5s ease;
        }

        .welcome-icon-wrap {
          width: 72px;
          height: 72px;
          border-radius: 50%;
          background: var(--color-accent-soft);
          display: flex;
          align-items: center;
          justify-content: center;
          margin: 0 auto 20px;
          position: relative;
        }

        .welcome-icon-wrap::after {
          content: '';
          position: absolute;
          inset: -6px;
          border-radius: 50%;
          border: 1.5px dashed var(--color-accent);
          opacity: 0.25;
          animation: welcome-spin-slow 20s linear infinite;
        }

        .welcome-icon-wrap .hash-icon {
          font-size: 30px;
          font-weight: 700;
          color: var(--color-accent-bright);
          font-family: var(--font-display);
        }

        .welcome-title {
          font-family: var(--font-display);
          font-size: 26px;
          font-weight: 700;
          color: var(--color-text-primary);
          margin-bottom: 8px;
          letter-spacing: -0.01em;
        }

        .welcome-sub {
          color: var(--color-text-muted);
          font-size: 14px;
          line-height: 1.6;
          margin-bottom: 28px;
          font-family: var(--font-sans);
        }

        .welcome-actions {
          display: flex;
          gap: 10px;
          justify-content: center;
          flex-wrap: wrap;
        }

        .welcome-btn {
          display: flex;
          align-items: center;
          gap: 7px;
          padding: 9px 16px;
          border-radius: var(--radius-md);
          font-size: 13px;
          font-weight: 500;
          cursor: pointer;
          transition: background 0.15s, transform 0.1s, box-shadow 0.15s;
          border: none;
          font-family: var(--font-sans);
        }

        .welcome-btn:active {
          transform: scale(0.97);
        }

        .welcome-btn.primary {
          background: var(--color-accent);
          color: var(--color-on-accent);
          box-shadow: 0 2px 12px color-mix(in srgb, var(--color-accent) 22%, transparent);
        }

        .welcome-btn.primary:hover {
          background: var(--color-accent-hover);
          box-shadow: 0 4px 20px color-mix(in srgb, var(--color-accent) 22%, transparent);
        }

        .welcome-btn.secondary {
          background: var(--color-bg-light);
          color: var(--color-text-muted);
        }

        .welcome-btn.secondary:hover {
          background: var(--color-bg-lighter);
          color: var(--color-text-primary);
        }

        .welcome-btn svg {
          width: 15px;
          height: 15px;
        }
      `}</style>

      <div className="welcome-state">
        <div className="welcome-icon-wrap">
          <span className="hash-icon">#</span>
        </div>
        <h1 className="welcome-title">Welcome to #{channelName}</h1>
        <p className="welcome-sub">
          This is the start of the #{channelName} channel. Invite your squad and
          get the conversation going.
        </p>
        <div className="welcome-actions">
          <button className="welcome-btn primary" onClick={onInvite}>
            <UserPlus size={15} />
            Invite friends
          </button>
          <button className="welcome-btn secondary" onClick={onEditTopic}>
            <Pencil size={15} />
            Edit topic
          </button>
          <button className="welcome-btn secondary" onClick={onSetRules}>
            <Lock size={15} />
            Set rules
          </button>
        </div>
      </div>
    </>
  );
};

export default WelcomeState;
