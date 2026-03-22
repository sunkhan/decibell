import { useVoiceStore } from "../../stores/voiceStore";
import { useAuthStore } from "../../stores/authStore";
import { stringToColor } from "../../utils/colors";

interface Props {
  /** If provided, render these usernames (for non-connected channels). Otherwise use connected participants. */
  usernames?: string[];
  /** Channel ID for looking up user states when not connected */
  channelId?: string;
}

function MuteIcon() {
  return (
    <svg className="ml-auto h-3 w-3 flex-shrink-0 text-danger" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
      <line x1="1" y1="1" x2="23" y2="23" />
      <path d="M9 9v3a3 3 0 0 0 5.12 2.12M15 9.34V4a3 3 0 0 0-5.94-.6" />
      <path d="M17 16.95A7 7 0 0 1 5 12v-2m14 0v2c0 .76-.13 1.49-.35 2.17" />
      <line x1="12" y1="19" x2="12" y2="23" />
      <line x1="8" y1="23" x2="16" y2="23" />
    </svg>
  );
}

function DeafenIcon() {
  return (
    <svg className="ml-auto h-3 w-3 flex-shrink-0 text-danger" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
      <path d="M3 14h3a2 2 0 0 1 2 2v3a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-7a9 9 0 0 1 18 0v7a2 2 0 0 1-2 2h-1a2 2 0 0 1-2-2v-3a2 2 0 0 1 2-2h3" />
      <line x1="1" y1="1" x2="23" y2="23" />
    </svg>
  );
}

export default function VoiceParticipantList({ usernames, channelId }: Props) {
  const participants = useVoiceStore((s) => s.participants);
  const speakingUsers = useVoiceStore((s) => s.speakingUsers);
  const isMuted = useVoiceStore((s) => s.isMuted);
  const isDeafened = useVoiceStore((s) => s.isDeafened);
  const channelUserStates = useVoiceStore((s) => s.channelUserStates);
  const localUsername = useAuthStore((s) => s.username);

  // If usernames provided (non-connected channel), show those with state from channelUserStates
  if (usernames) {
    if (usernames.length === 0) return null;
    const states = channelId ? channelUserStates[channelId] ?? {} : {};
    return (
      <div className="space-y-0.5 pb-1 pl-5">
        {usernames.map((u) => {
          const userState = states[u];
          return (
            <div
              key={u}
              className="flex items-center gap-1.5 rounded px-1.5 py-0.5 text-[11px] text-text-secondary"
            >
              <div
                className="flex h-[18px] w-[18px] flex-shrink-0 items-center justify-center rounded-md text-[8px] font-bold text-white"
                style={{ backgroundColor: stringToColor(u) }}
              >
                {u.charAt(0).toUpperCase()}
              </div>
              <span className="truncate">{u}</span>
              {userState?.isDeafened ? <DeafenIcon /> : userState?.isMuted ? <MuteIcon /> : null}
            </div>
          );
        })}
      </div>
    );
  }

  if (participants.length === 0) return null;

  return (
    <div className="space-y-0.5 pb-1 pl-5">
      {participants.map((p) => {
        const isSpeaking = speakingUsers.includes(p.username);
        const isLocal = p.username === localUsername;
        const userMuted = isLocal ? isMuted : p.isMuted;
        const userDeafened = isLocal ? isDeafened : p.isDeafened;
        const color = stringToColor(p.username);
        return (
          <div
            key={p.username}
            className="flex items-center gap-1.5 rounded px-1.5 py-0.5 text-[11px] text-text-secondary"
          >
            <div
              className="flex h-[18px] w-[18px] flex-shrink-0 items-center justify-center rounded-md text-[8px] font-bold text-white transition-shadow duration-200"
              style={{
                backgroundColor: color,
                boxShadow: isSpeaking
                  ? "0 0 0 2px #4aaa77, 0 0 6px #4aaa77"
                  : "none",
              }}
            >
              {p.username.charAt(0).toUpperCase()}
            </div>
            <span className="truncate">{p.username}</span>
            {userDeafened ? <DeafenIcon /> : userMuted ? <MuteIcon /> : null}
          </div>
        );
      })}
    </div>
  );
}
