import * as dns from "node:dns";
import { BlockList, isIP, type LookupFunction } from "node:net";

// Destination guard for main-process HTTP that other people's message
// text drives (link previews: the page, its redirects, og:image, the
// oEmbed endpoint). Such a request must only ever reach the public
// internet — never this machine, the LAN, the cloud metadata address or
// anything that tunnels to them.
//
// Two halves, both needed:
//   - `isPublicAddress` classifies one address with `net.BlockList`,
//     which also matches every spelling of an IPv4-mapped IPv6 address
//     (`::ffff:7f00:1`, `::ffff:127.0.0.1`) against the IPv4 rules — the
//     hand-rolled check this replaces missed the WHATWG form URLs
//     normalise `[::ffff:127.0.0.1]` to;
//   - `publicOnlyLookup` is the `lookup` an http(s).request connects
//     through, so the address checked is the address connected to. A
//     resolve-then-fetch check let a rebinding host answer differently
//     the second time. IP literals never reach a lookup (net connects
//     to them directly), so callers check those with `isPublicAddress`.
//
// No electron imports: kept standalone so it can be exercised with plain
// node.

const V4_BLOCKED: [string, number][] = [
  ["0.0.0.0", 8], // "this network"
  ["10.0.0.0", 8], // private
  ["100.64.0.0", 10], // CGNAT
  ["127.0.0.0", 8], // loopback
  ["169.254.0.0", 16], // link-local (cloud metadata)
  ["172.16.0.0", 12], // private
  ["192.0.0.0", 24], // IETF protocol assignments
  ["192.0.2.0", 24], // TEST-NET-1
  ["192.88.99.0", 24], // 6to4 relay anycast
  ["192.168.0.0", 16], // private
  ["198.18.0.0", 15], // benchmarking
  ["198.51.100.0", 24], // TEST-NET-2
  ["203.0.113.0", 24], // TEST-NET-3
  ["224.0.0.0", 4], // multicast
  ["240.0.0.0", 4], // reserved, broadcast
];

// IPv4-mapped (::ffff:0:0/96) is deliberately absent: BlockList checks
// those against the IPv4 rules above (a rule here would block all IPv4).
const V6_BLOCKED: [string, number][] = [
  ["::", 96], // unspecified, loopback, IPv4-compatible (::7f00:1)
  ["::ffff:0:0:0", 96], // IPv4-translated
  ["64:ff9b::", 96], // NAT64
  ["64:ff9b:1::", 48], // local-use NAT64
  ["100::", 64], // discard-only
  ["2001::", 32], // Teredo (embeds an IPv4 address)
  ["2001:db8::", 32], // documentation
  ["2002::", 16], // 6to4 (embeds an IPv4 address)
  ["fc00::", 7], // unique-local
  ["fe80::", 10], // link-local
  ["fec0::", 10], // site-local (deprecated)
  ["ff00::", 8], // multicast
];

const blocked = new BlockList();
for (const [net, prefix] of V4_BLOCKED) blocked.addSubnet(net, prefix, "ipv4");
for (const [net, prefix] of V6_BLOCKED) blocked.addSubnet(net, prefix, "ipv6");

/// True only for a well-formed address outside every blocked range.
/// Anything unparseable, or carrying an IPv6 zone id, counts as private.
export function isPublicAddress(ip: string): boolean {
  if (ip.includes("%")) return false;
  const v = isIP(ip);
  if (v === 0) return false;
  try {
    return !blocked.check(ip, v === 4 ? "ipv4" : "ipv6");
  } catch {
    return false;
  }
}

/// `lookup` for http(s).request: resolves like `dns.lookup`, and fails
/// the connection when any address the name resolves to is not public
/// (one private answer is enough — a rebinding name mixes them).
export const publicOnlyLookup: LookupFunction = (hostname, options, callback) => {
  dns.lookup(hostname, { ...options, all: true }, (err, addresses) => {
    if (err) {
      callback(err, "");
      return;
    }
    const list = addresses as dns.LookupAddress[];
    if (list.length === 0 || list.some((a) => !isPublicAddress(a.address))) {
      const e: NodeJS.ErrnoException = new Error(`${hostname} resolves to a non-public address`);
      e.code = "ENOTPUBLIC";
      callback(e, "");
      return;
    }
    if (options.all) callback(null, list);
    else callback(null, list[0].address, list[0].family);
  });
};
