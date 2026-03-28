import { useVoiceStore } from "../../stores/voiceStore";
import { useAuthStore } from "../../stores/authStore";
import { stringToGradient } from "../../utils/colors";

interface Props {
  usernames?: string[];
  channelId?: string;
}

function MuteIcon() {
  return (
    <svg className="ml-auto h-3.5 w-3.5 shrink-0 text-error" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
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
    <svg className="ml-auto h-3.5 w-3.5 shrink-0 text-error" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
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

  if (usernames) {
    if (usernames.length === 0) return null;
    const states = channelId ? channelUserStates[channelId] ?? {} : {};
    return (
      <div className="space-y-0.5 pb-1 pl-5">
        {usernames.map((u) => {
          const userState = states[u];
          return (
            <div key={u} className="flex items-center gap-2 rounded px-1.5 py-1 text-[12px] text-text-secondary">
              <div
                className="flex h-[22px] w-[22px] shrink-0 items-center justify-center rounded-md text-[10px] font-bold text-white"
                style={{ background: stringToGradient(u) }}
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
        return (
          <div key={p.username} className="flex items-center gap-2 rounded px-1.5 py-1 text-[12px] text-text-secondary">
            <div
              className="flex h-[22px] w-[22px] shrink-0 items-center justify-center rounded-md text-[10px] font-bold text-white transition-shadow duration-200"
              style={{
                background: stringToGradient(p.username),
                boxShadow: isSpeaking ? "0 0 0 2px #3fb950, 0 0 6px #3fb950" : "none",
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
