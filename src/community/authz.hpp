#pragma once
// Authorization resolver for the community server (permissions v2).
//
// One place that answers "may `user` do `action` (in `channel`, to
// `target`)?". Handlers used to inline `db->has_permission(user, bit)`
// plus ad-hoc owner / hierarchy checks fifteen times over; adding the
// channel dimension (per-channel overwrites) would have meant editing
// every one of them. Now a handler does
//
//     if (auto a = authz.check(Action::SendMessage, {user, channel}); !a) {
//         fail(a.reason); return;
//     }
//
// and the rules live here. See
// docs/superpowers/specs/2026-08-22-permissions-v2-design.md.
#include <chrono>
#include <cstdint>
#include <ctime>
#include <optional>
#include <string>
#include <vector>

#include "db.hpp"

namespace chatproj {

enum class Action {
    // Server-wide (base permissions; overwrites don't apply).
    ManageServer,       // picture, name/description
    ManageRoles,        // role CRUD + assignment (hierarchy checked by caller on the role)
    ManageInvites,
    ViewBans,
    KickMember,         // + can_moderate(target)
    BanMember,          // + can_moderate(target)
    UnbanMember,
    ManageNicknameOf,   // + can_moderate(target); self always allowed
    CreateChannel,      // MANAGE_CHANNELS server-wide (no channel yet)
    ReorderChannels,    // MANAGE_CHANNELS server-wide
    ViewAuditLog,
    TimeoutMember,      // MODERATE_MEMBERS + can_moderate(target), never the owner
    VoiceModerate,      // VOICE_MODERATE + can_moderate(target)
    TransferOwnership,  // owner only

    // Channel-scoped (channel_permissions: base → overwrites).
    ViewChannel,
    SendMessage,
    AttachFiles,
    ReadHistory,
    ConnectVoice,
    Stream,
    ManageChannel,      // retention, bitrate, rename, delete
    WipeChannel,
    DeleteOthersMessage,
    ManageOverwrites,   // MANAGE_ROLES in the channel
    ViewOverwrites,     // MANAGE_ROLES or MANAGE_CHANNELS in the channel
};

struct AuthCtx {
    std::string user;
    std::string channel_id;   // channel-scoped actions
    std::string target;       // target username for moderation actions
};

struct AuthResult {
    bool ok = false;
    std::string reason;       // user-facing, uniform across handlers
    explicit operator bool() const { return ok; }
};

class Authorizer {
public:
    explicit Authorizer(const CommunityDb& db) : db_(db) {}

    // Hierarchy: strictly higher level only. Level = highest ASSIGNED role
    // position (0 with none), owner = INT32_MAX. Two members who only hold
    // `everyone` can't moderate each other even if `everyone` carries
    // KICK_MEMBERS — grant a real role. ADMINISTRATOR never bypasses this.
    bool can_moderate(const std::string& actor, const std::string& target) const {
        return db_.member_level(actor) > db_.member_level(target);
    }

    // Active timeout end (unix seconds) or 0. A timed-out member can't send,
    // attach, join voice or stream; everything else (reading, presence)
    // keeps working. The owner can't be timed out (can_moderate forbids
    // it) so no bypass is needed here.
    int64_t timeout_until(const std::string& user) const {
        auto m = db_.get_member(user);
        if (!m || m->timed_out_until == 0) return 0;
        const int64_t now = std::chrono::duration_cast<std::chrono::seconds>(
            std::chrono::system_clock::now().time_since_epoch()).count();
        return m->timed_out_until > now ? m->timed_out_until : 0;
    }

    uint64_t channel_permissions(const std::string& user, const std::string& channel_id) const {
        return db_.channel_permissions(user, channel_id);
    }

    // Channels the user can VIEW, in display order. Category rows are
    // always included so the sidebar keeps its structure.
    std::vector<DbChannel> visible_channels(const std::string& user) const {
        std::vector<DbChannel> out;
        for (auto& ch : db_.list_channels()) {
            if (ch.type == 2 || (channel_permissions(user, ch.id) & perms::kViewChannel)) {
                out.push_back(std::move(ch));
            }
        }
        return out;
    }

    // Purely passive actions stay available while timed out — a benched
    // member can still read. Everything else is suppressed (M4).
    static bool is_passive_action(Action a) {
        switch (a) {
            case Action::ViewChannel:
            case Action::ReadHistory:
            case Action::ViewBans:
            case Action::ViewAuditLog:
            case Action::ViewOverwrites:
                return true;
            default:
                return false;
        }
    }

    AuthResult check(Action action, const AuthCtx& ctx) const {
        // A timed-out member is benched: every non-passive action is denied
        // while the timeout is active — not just sending/voice, but any
        // management or moderation power they hold (M4). The owner can never
        // be timed out (can_moderate forbids it), so no bypass is needed.
        if (!is_passive_action(action)) {
            if (auto t = timed_out(ctx)) return *t;
        }
        switch (action) {
            // ---- server-wide ----
            case Action::ManageServer:
                return base(ctx, perms::kManageServer, "You don't have permission to manage this server.");
            case Action::ManageRoles:
                return base(ctx, perms::kManageRoles, "You don't have permission to manage roles.");
            case Action::ManageInvites:
                return base(ctx, perms::kManageInvites, "You don't have permission to manage invites.");
            case Action::ViewBans:
                return base(ctx, perms::kBanMembers, "You don't have permission to view bans.");
            case Action::UnbanMember:
                return base(ctx, perms::kBanMembers, "You don't have permission to unban members.");
            case Action::CreateChannel:
            case Action::ReorderChannels:
                return base(ctx, perms::kManageChannels, "You don't have permission to manage channels.");
            case Action::ViewAuditLog:
                return base(ctx, perms::kViewAuditLog, "You don't have permission to view the audit log.");
            case Action::TimeoutMember: {
                auto r = base(ctx, perms::kModerateMembers, "You don't have permission to time out members.");
                if (!r) return r;
                if (ctx.target == db_.owner()) return deny("The server owner can't be timed out.");
                if (ctx.target == ctx.user) return deny("You can't time yourself out.");
                if (!can_moderate(ctx.user, ctx.target)) {
                    return deny("You can't time out a member with an equal or higher role.");
                }
                return ok();
            }
            case Action::VoiceModerate: {
                auto r = base(ctx, perms::kVoiceModerate, "You don't have permission to moderate voice.");
                if (!r) return r;
                if (ctx.target == db_.owner()) return deny("The server owner can't be voice-moderated.");
                if (!can_moderate(ctx.user, ctx.target)) {
                    return deny("You can't voice-moderate a member with an equal or higher role.");
                }
                return ok();
            }
            case Action::TransferOwnership:
                return ctx.user == db_.owner() ? ok() : deny("Only the server owner can transfer ownership.");
            case Action::KickMember: {
                auto r = base(ctx, perms::kKickMembers, "You don't have permission to kick members.");
                if (!r) return r;
                return moderation_target(ctx, "kick");
            }
            case Action::BanMember: {
                auto r = base(ctx, perms::kBanMembers, "You don't have permission to ban members.");
                if (!r) return r;
                return moderation_target(ctx, "ban");
            }
            case Action::ManageNicknameOf: {
                if (ctx.target == ctx.user) return ok();
                auto r = base(ctx, perms::kManageNicknames, "You don't have permission to manage nicknames.");
                if (!r) return r;
                if (!can_moderate(ctx.user, ctx.target)) {
                    return deny("You can't change the nickname of a member with an equal or higher role.");
                }
                return ok();
            }

            // ---- channel-scoped ----
            case Action::ViewChannel:
                return channel(ctx, perms::kViewChannel, "You can't see this channel.");
            case Action::SendMessage:
                return channel(ctx, perms::kSendMessages, "You don't have permission to send messages here.");
            case Action::AttachFiles:
                return channel(ctx, perms::kAttachFiles, "You don't have permission to attach files here.");
            case Action::ReadHistory:
                return channel(ctx, perms::kReadHistory, "You don't have permission to read this channel's history.");
            case Action::ConnectVoice:
                return channel(ctx, perms::kConnectVoice, "You don't have permission to join this voice channel.");
            case Action::Stream:
                return channel(ctx, perms::kStream, "You don't have permission to stream here.");
            case Action::ManageChannel:
                return channel(ctx, perms::kManageChannels, "You don't have permission to edit this channel.");
            case Action::WipeChannel:
                return channel(ctx, perms::kManageChannels, "You don't have permission to wipe channel history.");
            case Action::DeleteOthersMessage:
                return channel(ctx, perms::kManageMessages, "You don't have permission to delete this message.");
            case Action::ManageOverwrites:
                return channel(ctx, perms::kManageRoles, "You don't have permission to edit this channel's permissions.");
            case Action::ViewOverwrites: {
                const uint64_t p = channel_permissions(ctx.user, ctx.channel_id);
                if ((p & perms::kManageRoles) || (p & perms::kManageChannels)) return ok();
                return deny("You don't have permission to view this channel's permissions.");
            }
        }
        return deny("Not allowed.");
    }

private:
    static AuthResult ok() { return AuthResult{true, ""}; }
    static AuthResult deny(const char* why) { return AuthResult{false, why}; }

    // Non-empty denial when the user is timed out; empty (ok=false,
    // reason="") otherwise — callers use it as `if (auto t = ...) return t;`
    // via the operator bool on `ok`, so return a denial only when active.
    std::optional<AuthResult> timed_out(const AuthCtx& ctx) const {
        const int64_t until = timeout_until(ctx.user);
        if (until == 0) return std::nullopt;
        return AuthResult{false, "You are timed out until " + format_time(until) + "."};
    }
    static std::string format_time(int64_t ts) {
        std::time_t t = static_cast<std::time_t>(ts);
        char buf[32];
        std::tm tm{};
#ifdef _WIN32
        gmtime_s(&tm, &t);
#else
        gmtime_r(&t, &tm);
#endif
        std::strftime(buf, sizeof(buf), "%Y-%m-%d %H:%M UTC", &tm);
        return buf;
    }

    AuthResult base(const AuthCtx& ctx, uint64_t bit, const char* why) const {
        return db_.has_permission(ctx.user, bit) ? ok() : deny(why);
    }
    AuthResult channel(const AuthCtx& ctx, uint64_t bit, const char* why) const {
        return (channel_permissions(ctx.user, ctx.channel_id) & bit) == bit ? ok() : deny(why);
    }
    AuthResult moderation_target(const AuthCtx& ctx, const char* verb) const {
        if (ctx.target == db_.owner()) {
            return deny(verb[0] == 'k' ? "Cannot kick the server owner."
                                       : "Cannot ban the server owner.");
        }
        if (ctx.target == ctx.user) {
            return deny(verb[0] == 'k' ? "Use leave to remove yourself." : "Cannot ban yourself.");
        }
        if (!can_moderate(ctx.user, ctx.target)) {
            return deny(verb[0] == 'k'
                ? "You can't kick a member with an equal or higher role."
                : "You can't ban a member with an equal or higher role.");
        }
        return ok();
    }

    const CommunityDb& db_;
};

} // namespace chatproj
