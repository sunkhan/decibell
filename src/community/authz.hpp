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
#include <cstdint>
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

    AuthResult check(Action action, const AuthCtx& ctx) const {
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
