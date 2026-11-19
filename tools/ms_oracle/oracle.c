/* libopus multistream oracle: encode a synthetic multichannel signal with
 * libopus's multistream encoder, decode the packets with libopus's
 * multistream decoder (float), and write
 *   <name>.msp   raw packets + header (see write_msp)
 *   <name>.ref   libopus decoded float PCM, interleaved, every sample of every
 *                packet (no pre-skip / end trim)
 *   <name>.opus  Ogg Opus file of the same packets (OpusHead with the mapping
 *                family + table, OpusTags, granules with pre-skip and an end
 *                trim)
 *
 * Usage: oracle NAME CHANNELS FAMILY MODE BITRATE FRAME_MS [MAP...]
 *   FAMILY 1: surround layout from libopus.
 *   FAMILY 255: MAP is "STREAMS COUPLED m0 m1 ... m(ch-1)".
 *   MODE: auto | celt | silk | hybrid
 * No opus headers on this box: prototypes declared here, linked against
 * libopus.so.0 directly. */
#include <math.h>
#include <stdarg.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

typedef struct OpusMSEncoder OpusMSEncoder;
typedef struct OpusMSDecoder OpusMSDecoder;
OpusMSEncoder *opus_multistream_surround_encoder_create(int32_t Fs, int channels, int mapping_family,
                                                        int *streams, int *coupled_streams,
                                                        unsigned char *mapping, int application,
                                                        int *error);
OpusMSEncoder *opus_multistream_encoder_create(int32_t Fs, int channels, int streams,
                                               int coupled_streams, const unsigned char *mapping,
                                               int application, int *error);
int opus_multistream_encode_float(OpusMSEncoder *st, const float *pcm, int frame_size,
                                  unsigned char *data, int32_t max_data_bytes);
void opus_multistream_encoder_destroy(OpusMSEncoder *st);
int opus_multistream_encoder_ctl(OpusMSEncoder *st, int request, ...);
OpusMSDecoder *opus_multistream_decoder_create(int32_t Fs, int channels, int streams,
                                               int coupled_streams, const unsigned char *mapping,
                                               int *error);
int opus_multistream_decode_float(OpusMSDecoder *st, const unsigned char *data, int32_t len,
                                  float *pcm, int frame_size, int decode_fec);
void opus_multistream_decoder_destroy(OpusMSDecoder *st);
const char *opus_strerror(int error);
const char *opus_get_version_string(void);
typedef struct OpusEncoder OpusEncoder;
int opus_encoder_ctl(OpusEncoder *st, int request, ...);
#define OPUS_MULTISTREAM_GET_ENCODER_STATE_REQUEST 5120

#define OPUS_APPLICATION_VOIP 2048
#define OPUS_APPLICATION_AUDIO 2049
#define OPUS_SET_BITRATE_REQUEST 4002
#define OPUS_SET_BANDWIDTH_REQUEST 4008
#define OPUS_SET_COMPLEXITY_REQUEST 4010
#define OPUS_SET_SIGNAL_REQUEST 4024
#define OPUS_GET_LOOKAHEAD_REQUEST 4027
#define OPUS_SET_FORCE_MODE_REQUEST 11002
#define OPUS_SIGNAL_VOICE 3001
#define OPUS_SIGNAL_MUSIC 3002
#define OPUS_BANDWIDTH_WIDEBAND 1103
#define OPUS_BANDWIDTH_FULLBAND 1105
#define MODE_SILK_ONLY 1000
#define MODE_HYBRID 1001
#define MODE_CELT_ONLY 1002

#define FS 48000
#define MAX_CH 8

/* ---- Ogg page writer -------------------------------------------------- */
static uint32_t crc_tab[256];
static void crc_init(void) {
    for (uint32_t i = 0; i < 256; i++) {
        uint32_t r = i << 24;
        for (int k = 0; k < 8; k++) r = (r & 0x80000000u) ? (r << 1) ^ 0x04c11db7u : (r << 1);
        crc_tab[i] = r;
    }
}
static uint32_t crc32_ogg(const unsigned char *p, size_t n) {
    uint32_t c = 0;
    for (size_t i = 0; i < n; i++) c = (c << 8) ^ crc_tab[((c >> 24) ^ p[i]) & 0xff];
    return c;
}

typedef struct {
    FILE *f;
    uint32_t serial, seq;
    unsigned char seg[255];
    int nseg;
    unsigned char body[255 * 255];
    int nbody;
} Ogg;

static void put32(unsigned char *p, uint32_t v) {
    p[0] = v; p[1] = v >> 8; p[2] = v >> 16; p[3] = v >> 24;
}
static void ogg_flush(Ogg *o, int64_t granule, int flags) {
    unsigned char h[27 + 255];
    memcpy(h, "OggS", 4);
    h[4] = 0;
    h[5] = flags;
    for (int i = 0; i < 8; i++) h[6 + i] = (uint64_t)granule >> (8 * i);
    put32(h + 14, o->serial);
    put32(h + 18, o->seq++);
    put32(h + 22, 0);
    h[26] = o->nseg;
    memcpy(h + 27, o->seg, o->nseg);
    int hl = 27 + o->nseg;
    unsigned char *page = malloc(hl + o->nbody);
    memcpy(page, h, hl);
    memcpy(page + hl, o->body, o->nbody);
    put32(page + 22, crc32_ogg(page, hl + o->nbody));
    fwrite(page, 1, hl + o->nbody, o->f);
    free(page);
    o->nseg = 0;
    o->nbody = 0;
}
static int ogg_fits(Ogg *o, int len) { return o->nseg + len / 255 + 1 <= 255; }
static void ogg_add(Ogg *o, const unsigned char *p, int len) {
    int left = len;
    while (left >= 255) { o->seg[o->nseg++] = 255; left -= 255; }
    o->seg[o->nseg++] = left;
    memcpy(o->body + o->nbody, p, len);
    o->nbody += len;
}

/* ---- signal ----------------------------------------------------------- */
static uint32_t lcg = 12345;
static float noise(void) {
    lcg = lcg * 1664525u + 1013904223u;
    return ((int32_t)lcg) / 2147483648.0f;
}
static void synth(float *pcm, int n, int ch) {
    for (int i = 0; i < n; i++) {
        double t = (double)i / FS;
        for (int c = 0; c < ch; c++) {
            double f = 180.0 * (c + 1) + 37.0 * c * c;
            double am = 0.6 + 0.4 * sin(2 * M_PI * (0.7 + 0.3 * c) * t);
            double v = 0.22 * am * sin(2 * M_PI * f * t + c) +
                       0.06 * sin(2 * M_PI * (f * 3.1 + 400 * t) * t);
            /* a short burst per channel to stir transients */
            if (((i / 4800) % ch) == c && (i % 4800) < 480) v += 0.3 * noise();
            pcm[i * ch + c] = (float)(v + 0.01 * noise());
        }
    }
}

static int toc_mode(unsigned char toc) {
    if (toc & 0x80) return MODE_CELT_ONLY;
    if ((toc & 0x60) == 0x60) return MODE_HYBRID;
    return MODE_SILK_ONLY;
}

/* Byte length of a self-delimited sub-packet (RFC 6716 appendix B). */
static int rd_size(const unsigned char *d, int *pos) {
    int v = d[*pos];
    if (v < 252) { *pos += 1; return v; }
    v += 4 * d[*pos + 1];
    *pos += 2;
    return v;
}
static int sd_len(const unsigned char *d) {
    int pos = 1, code = d[0] & 3;
    if (code == 0) { int s = rd_size(d, &pos); return pos + s; }
    if (code == 1) { int s = rd_size(d, &pos); return pos + 2 * s; }
    if (code == 2) { int a = rd_size(d, &pos); int b = rd_size(d, &pos); return pos + a + b; }
    int c = d[pos++], count = c & 63, pad = 0;
    if (c & 0x40) { int p; do { p = d[pos++]; pad += p == 255 ? 254 : p; } while (p == 255); }
    int sum = 0;
    if (c & 0x80) {
        for (int i = 0; i < count - 1; i++) sum += rd_size(d, &pos);
        sum += rd_size(d, &pos);
    } else {
        sum = count * rd_size(d, &pos);
    }
    return pos + sum + pad;
}

int main(int argc, char **argv) {
    if (argc < 7) {
        fprintf(stderr, "usage: oracle NAME CH FAMILY MODE BITRATE FRAME_MS [STREAMS COUPLED MAP...]\n");
        return 2;
    }
    crc_init();
    const char *name = argv[1];
    int ch = atoi(argv[2]), family = atoi(argv[3]);
    const char *mode = argv[4];
    int bitrate = atoi(argv[5]);
    double frame_ms = atof(argv[6]);
    int frame = (int)(FS * frame_ms / 1000.0);
    int streams = 0, coupled = 0, err = 0;
    unsigned char mapping[MAX_CH];
    int app = strcmp(mode, "silk") == 0 ? OPUS_APPLICATION_VOIP : OPUS_APPLICATION_AUDIO;
    OpusMSEncoder *enc;
    /* "s:MODE" = libopus's surround encoder (it forces coupled streams to
     * CELT and picks bandwidth itself; MODE applies to the mono streams).
     * Plain MODE on family 1 = the generic encoder with the same Vorbis
     * layout, so the forced mode reaches every stream. */
    int surround = strncmp(mode, "s:", 2) == 0;
    if (surround) mode += 2;
    if (strcmp(mode, "silk") == 0) app = OPUS_APPLICATION_VOIP;
    if (family == 1) {
        enc = opus_multistream_surround_encoder_create(FS, ch, 1, &streams, &coupled, mapping, app, &err);
        if (enc && !surround) {
            opus_multistream_encoder_destroy(enc);
            enc = opus_multistream_encoder_create(FS, ch, streams, coupled, mapping, app, &err);
        }
    } else {
        if (argc < 9 + ch) { fprintf(stderr, "family 255 needs STREAMS COUPLED MAP\n"); return 2; }
        streams = atoi(argv[7]);
        coupled = atoi(argv[8]);
        for (int c = 0; c < ch; c++) mapping[c] = atoi(argv[9 + c]);
        enc = opus_multistream_encoder_create(FS, ch, streams, coupled, mapping, app, &err);
    }
    if (!enc) { fprintf(stderr, "encoder create: %s\n", opus_strerror(err)); return 1; }
    if (opus_multistream_encoder_ctl(enc, OPUS_SET_BITRATE_REQUEST, bitrate) ||
        opus_multistream_encoder_ctl(enc, OPUS_SET_COMPLEXITY_REQUEST, 10)) {
        fprintf(stderr, "ms ctl failed\n");
        return 1;
    }
    /* The multistream ctl does not forward FORCE_MODE: set it per stream. */
    for (int s = 0; s < streams; s++) {
        OpusEncoder *e = NULL;
        int r = opus_multistream_encoder_ctl(enc, OPUS_MULTISTREAM_GET_ENCODER_STATE_REQUEST, s, &e);
        if (r != 0 || !e) { fprintf(stderr, "get encoder state: %d\n", r); return 1; }
        int r1 = 0, r2 = 0, r3 = 0;
        if (strcmp(mode, "celt") == 0) {
            r1 = opus_encoder_ctl(e, OPUS_SET_FORCE_MODE_REQUEST, MODE_CELT_ONLY);
        } else if (strcmp(mode, "silk") == 0) {
            r1 = opus_encoder_ctl(e, OPUS_SET_FORCE_MODE_REQUEST, MODE_SILK_ONLY);
            r2 = opus_encoder_ctl(e, OPUS_SET_SIGNAL_REQUEST, OPUS_SIGNAL_VOICE);
            r3 = opus_encoder_ctl(e, OPUS_SET_BANDWIDTH_REQUEST, OPUS_BANDWIDTH_WIDEBAND);
        } else if (strcmp(mode, "hybrid") == 0) {
            r1 = opus_encoder_ctl(e, OPUS_SET_FORCE_MODE_REQUEST, MODE_HYBRID);
            r3 = opus_encoder_ctl(e, OPUS_SET_BANDWIDTH_REQUEST, OPUS_BANDWIDTH_FULLBAND);
        }
        if (r1 || r2 || r3) { fprintf(stderr, "ctl: %d %d %d\n", r1, r2, r3); return 1; }
    }
    /* CBR=1: constant bitrate (libopus pads the last stream to the size). */
    if (getenv("CBR") && opus_multistream_encoder_ctl(enc, 4006 /* OPUS_SET_VBR */, 0)) {
        fprintf(stderr, "vbr ctl failed\n");
        return 1;
    }
    int lookahead = 0;
    opus_multistream_encoder_ctl(enc, OPUS_GET_LOOKAHEAD_REQUEST, &lookahead);

    /* Default 1.5 s plus a ragged tail so the final granule trims the last
     * packet; DUR_MS overrides (small committed fixtures). */
    int input_len = getenv("DUR_MS") ? FS / 1000 * atoi(getenv("DUR_MS")) + 77 : FS * 3 / 2 + 123;
    /* DEC_FS: the rate libopus decodes the reference at (and the .msp says). */
    int dec_fs = getenv("DEC_FS") ? atoi(getenv("DEC_FS")) : FS;
    int nframes = (input_len + lookahead + frame - 1) / frame;
    int total = nframes * frame;
    float *pcm = calloc((size_t)total * ch, sizeof(float));
    synth(pcm, input_len, ch);

    /* DECODE_CH=n: the stream header (and decoder) declare only the first n
     * outputs, so some decoded channels go unmapped -- a layout libopus's
     * encoder will not create but its decoder accepts. */
    int dch = getenv("DECODE_CH") ? atoi(getenv("DECODE_CH")) : ch;
    OpusMSDecoder *dec = opus_multistream_decoder_create(dec_fs, dch, streams, coupled, mapping, &err);
    if (!dec) { fprintf(stderr, "decoder create: %s\n", opus_strerror(err)); return 1; }

    char path[512];
    snprintf(path, sizeof path, "%s.msp", name);
    FILE *msp = fopen(path, "wb");
    snprintf(path, sizeof path, "%s.ref", name);
    FILE *ref = fopen(path, "wb");
    snprintf(path, sizeof path, "%s.opus", name);
    Ogg ogg = {0};
    ogg.f = fopen(path, "wb");
    ogg.serial = 0x5eed0000u ^ (uint32_t)(ch * 977 + family * 31 + frame);

    /* .msp header */
    fwrite("MSPK", 1, 4, msp);
    unsigned char hb[16];
    put32(hb, dec_fs);
    hb[4] = dch; hb[5] = streams; hb[6] = coupled; hb[7] = family;
    fwrite(hb, 1, 8, msp);
    fwrite(mapping, 1, dch, msp);
    unsigned char pb[12];
    pb[0] = lookahead; pb[1] = lookahead >> 8;
    put32(pb + 2, input_len);
    put32(pb + 6, nframes);
    fwrite(pb, 1, 10, msp);

    /* OpusHead + OpusTags */
    unsigned char head[19 + 2 + MAX_CH];
    memcpy(head, "OpusHead", 8);
    head[8] = 1; head[9] = dch;
    head[10] = lookahead; head[11] = lookahead >> 8;
    put32(head + 12, FS);
    head[16] = 0; head[17] = 0;
    head[18] = family;
    head[19] = streams; head[20] = coupled;
    memcpy(head + 21, mapping, dch);
    ogg_add(&ogg, head, 21 + dch);
    ogg_flush(&ogg, 0, 2);
    const char *vendor = "opus-oracle";
    unsigned char tags[64];
    memcpy(tags, "OpusTags", 8);
    put32(tags + 8, strlen(vendor));
    memcpy(tags + 12, vendor, strlen(vendor));
    put32(tags + 12 + strlen(vendor), 0);
    ogg_add(&ogg, tags, 16 + strlen(vendor));
    ogg_flush(&ogg, 0, 0);

    unsigned char pkt[8000];
    float *out = malloc(sizeof(float) * 5760 * dch);
    int modes[3] = {0, 0, 0};
    int64_t gran = 0; /* granule = PCM samples decoded so far, pre-skip included */
    long bytes = 0;
    int pkts_on_page = 0;
    for (int k = 0; k < nframes; k++) {
        int n = opus_multistream_encode_float(enc, pcm + (size_t)k * frame * ch, frame, pkt, sizeof pkt);
        if (n < 0) { fprintf(stderr, "encode: %s\n", opus_strerror(n)); return 1; }
        for (int s = 0, off = 0; s < streams; s++) {
            modes[toc_mode(pkt[off]) - MODE_SILK_ONLY]++;
            if (s + 1 < streams) off += sd_len(pkt + off);
        }
        bytes += n;
        unsigned char lb[4];
        put32(lb, n);
        fwrite(lb, 1, 4, msp);
        fwrite(pkt, 1, n, msp);
        int d = opus_multistream_decode_float(dec, pkt, n, out, 5760, 0);
        if (d != frame / (FS / dec_fs)) { fprintf(stderr, "decode: %d (%s)\n", d, d < 0 ? opus_strerror(d) : ""); return 1; }
        fwrite(out, sizeof(float), (size_t)d * dch, ref);

        if (!ogg_fits(&ogg, n) || pkts_on_page == 25) {
            ogg_flush(&ogg, gran, 0);
            pkts_on_page = 0;
        }
        ogg_add(&ogg, pkt, n);
        pkts_on_page++;
        gran += frame;
    }
    /* Final granule: pre-skip + the real input length (trims the tail). */
    ogg_flush(&ogg, lookahead + input_len, 4);

    printf("%s ch=%d fam=%d streams=%d coupled=%d map=", name, dch, family, streams, coupled);
    for (int c = 0; c < dch; c++) printf("%s%d", c ? "," : "", mapping[c]);
    printf(" frame=%d preskip=%d frames=%d kbps=%.1f stream-frames(silk/hyb/celt)=%d/%d/%d  [%s]\n", frame,
           lookahead, nframes, bytes * 8.0 / (nframes * frame_ms), modes[0], modes[1], modes[2],
           opus_get_version_string());
    fclose(msp);
    fclose(ref);
    fclose(ogg.f);
    opus_multistream_encoder_destroy(enc);
    opus_multistream_decoder_destroy(dec);
    free(pcm);
    free(out);
    return 0;
}
