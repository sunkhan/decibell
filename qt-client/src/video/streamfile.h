// Reader for the spike's test streams: whole encoded frames as the receive path hands them on. Streams from disk so
// a 75 MB file isn't held in memory (and doesn't count against the client's working set). Loops at the end.
//
//   DBV1 (tools/streamgen, synthetic): "DBV1", u8 codec (1 H.264, 2 HEVC, 3 AV1), 3 zero bytes, u32 width,
//        u32 height, u32 fps; per frame u32 size, u32 flags (bit 0 keyframe), the bytes. Played at `fps`.
//   DBV2 (a real stream, recorded by the Electron client's debug build: native/src/media/stream_dump.rs):
//        "DBV2", u8 wire codec (1 / 2 H.264, 3 H.265, 4 AV1), 3 zero bytes, 12 zero bytes; per frame u32 size,
//        u32 flags (bit 0 keyframe, bit 1 description follows), u64 arrival µs, [u32 size, description], the
//        bytes. Played on its recorded arrival times; the description (hvcC / av1C) configures the decoder.
// Little-endian.
#pragma once

#include <cstdint>
#include <cstdio>
#include <string>
#include <vector>

namespace decibell::video {

enum class Codec : uint8_t { H264 = 1, Hevc = 2, Av1 = 3 };

struct FrameHeader {
    uint32_t size = 0;
    bool keyframe = false;
    bool wrapped = false;   // the file started over at this frame
    int64_t atUs = -1;      // DBV2: arrival since the first frame; DBV1: -1 (play at fps)
    std::vector<uint8_t> description;
};

class StreamFile
{
public:
    ~StreamFile()
    {
        if (m_f)
            fclose(m_f);
    }

    bool open(const std::wstring &path)
    {
        if (_wfopen_s(&m_f, path.c_str(), L"rb") != 0 || !m_f)
            return false;
        uint8_t magic[8];
        uint32_t dims[3];
        if (fread(magic, 1, 8, m_f) != 8 || fread(dims, 4, 3, m_f) != 3 || magic[0] != 'D' || magic[1] != 'B'
            || magic[2] != 'V' || (magic[3] != '1' && magic[3] != '2'))
            return false;
        version = magic[3] - '0';
        if (version == 1) {
            if (magic[4] < 1 || magic[4] > 3)
                return false;
            codec = Codec(magic[4]);
        } else {
            if (magic[4] < 1 || magic[4] > 4)
                return false;
            codec = magic[4] == 3 ? Codec::Hevc : magic[4] == 4 ? Codec::Av1 : Codec::H264;
        }
        width = dims[0];
        height = dims[1];
        fps = dims[2];
        return true;
    }

    // The next frame's header; then read() its bytes. Wraps to the first frame at the end.
    bool next(FrameHeader &h)
    {
        h.wrapped = false;
        if (!readHeader(h)) {
            fseek(m_f, kHeaderBytes, SEEK_SET);
            h.wrapped = true;
            if (!readHeader(h))
                return false;
        }
        return true;
    }
    bool read(uint8_t *dst, uint32_t size) { return fread(dst, 1, size, m_f) == size; }

    int version = 1;
    Codec codec = Codec::H264;
    uint32_t width = 0, height = 0, fps = 0; // DBV2: all 0 (the bitstream says; arrival times pace it)

private:
    bool readHeader(FrameHeader &h)
    {
        uint32_t head[2];
        if (fread(head, 4, 2, m_f) != 2)
            return false;
        h.size = head[0];
        h.keyframe = head[1] & 1;
        h.description.clear();
        h.atUs = -1;
        if (version == 2) {
            uint64_t at = 0;
            if (fread(&at, 8, 1, m_f) != 1)
                return false;
            h.atUs = int64_t(at);
            if (head[1] & 2) {
                uint32_t n = 0;
                if (fread(&n, 4, 1, m_f) != 1 || n > (1u << 20))
                    return false;
                h.description.resize(n);
                if (n && fread(h.description.data(), 1, n, m_f) != n)
                    return false;
            }
        }
        return true;
    }

    static constexpr long kHeaderBytes = 20;
    FILE *m_f = nullptr;
};

} // namespace decibell::video
