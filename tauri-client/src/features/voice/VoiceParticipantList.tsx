import { useVoiceStore } from "../../stores/voiceStore";
import { stringToColor } from "../../utils/colors";

export default function VoiceParticipantList() {
  const participants = useVoiceStore((s) => s.participants);
  const speakingUsers = useVoiceStore((s) => s.speakingUsers);

  if (participants.length === 0) return null;

  return (
    <div className="space-y-0.5 pb-1 pl-5">
      {participants.map((p) => {
        const isSpeaking = speakingUsers.includes(p.username);
        return (
          <div
            key={p.username}
            className="flex items-center gap-1.5 rounded px-1.5 py-0.5 text-[11px] text-text-secondary"
          >
            <div
              className="flex h-[18px] w-[18px] flex-shrink-0 items-center justify-center rounded-md text-[8px] font-bold text-white"
              style={{ backgroundColor: stringToColor(p.username) }}
            >
              {p.username.charAt(0).toUpperCase()}
            </div>
            <span className="truncate">{p.username}</span>
            {isSpeaking && (
              <span className="text-[14px] leading-none text-success">●</span>
            )}
            {p.isMuted && (
              <span className="text-[9px] text-danger">🔇</span>
            )}
          </div>
        );
      })}
    </div>
  );
}
