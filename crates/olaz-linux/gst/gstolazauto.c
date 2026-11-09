/*
 * gstolazauto.c — Minimal self-contained GStreamer 1.0 fallback plugin for Olaz.
 *
 * Provides:
 *   1. `autoaudiosink` (GstBin + GstStreamVolume): probes `pipewiresink` -> `pulsesink` ->
 *      `alsasink` -> `fakesink (sync=TRUE)` on `NULL -> READY` using an isolated temporary bus
 *      so headless/missing-daemon probes never pollute the pipeline bus, emits `child-added`
 *      for WebKit2GTK (`media.role` / `client-name`), and exposes sample-accurate `volume` + `mute`.
 *      Skipped automatically if system `libgstautodetect.so` is installed (unless
 *      `OLAZ_GST_FORCE_AUTO_SINKS=1` is set for testing).
 *   2. `autovideosink` (GstBin): probes `waylandsink` -> `glimagesink` -> `xvimagesink` ->
 *      `ximagesink` -> `fakesink (sync=TRUE)` when system `libgstautodetect.so` is absent.
 *   3. `olazwavparse` (rank `GST_RANK_MARGINAL`): push-mode RIFF/WAVE PCM & IEEE-float parser
 *      supporting `U8`, `S16LE`, `S24LE`, `S32LE`, `F32LE`, `F64LE`, `DURATION` queries, and
 *      `TIME` -> `BYTES` seeking so `data:audio/wav` and `.wav` voice notes play natively even
 *      when `gst-plugins-good` (`libgstwavparse.so`, rank `GST_RANK_PRIMARY`) is not installed.
 *
 * SPDX-License-Identifier: GPL-3.0-only
 */

#ifndef _GNU_SOURCE
#define _GNU_SOURCE
#endif

#include <dlfcn.h>
#include <math.h>
#include <gst/gst.h>
#include <gst/base/gstadapter.h>
#include <gst/base/gstbasetransform.h>
#include <gst/audio/audio.h>
#include <gst/audio/streamvolume.h>
#include <gst/video/video.h>
#include <sys/mman.h>
#include <unistd.h>
#ifdef OLAZ_HAVE_FFMPEG
#include <libavformat/avformat.h>
#include <libavcodec/avcodec.h>
#include <libswresample/swresample.h>
#include <libswscale/swscale.h>
#include <libavutil/avutil.h>
#include <libavutil/channel_layout.h>
#include <libavutil/opt.h>
#endif
#include <string.h>

static void olaz_ensure_dynamic_webkit_media_src (void);

#ifndef PACKAGE
#define PACKAGE "olaz"
#endif

/* ============================================================================
 * Helper: Detect whether system `libgstautodetect.so` (`gst-plugins-good`) exists
 * ============================================================================ */

static gboolean
olaz_system_has_autodetect_plugin (void)
{
  static const char *const kPaths[] = {
    "/usr/lib/gstreamer-1.0/libgstautodetect.so",
    "/usr/lib64/gstreamer-1.0/libgstautodetect.so",
    "/usr/lib/x86_64-linux-gnu/gstreamer-1.0/libgstautodetect.so",
    "/usr/lib/aarch64-linux-gnu/gstreamer-1.0/libgstautodetect.so",
    "/usr/local/lib/gstreamer-1.0/libgstautodetect.so",
    "/usr/local/lib64/gstreamer-1.0/libgstautodetect.so",
  };
  const char *force_env = g_getenv ("OLAZ_GST_FORCE_AUTO_SINKS");
  if (force_env != NULL && force_env[0] != '\0' && strcmp (force_env, "0") != 0)
    return FALSE;

  for (gsize i = 0; i < G_N_ELEMENTS (kPaths); i++) {
    if (g_file_test (kPaths[i], G_FILE_TEST_IS_REGULAR))
      return TRUE;
  }
  return FALSE;
}

/* Probe whether a factory name can transition cleanly to `GST_STATE_READY`
 * using a private `GstBus` so failed probes never leak error messages upstream. */
static gboolean
olaz_probe_sink_factory (const char *factory_name)
{
  GstElement *test_el = gst_element_factory_make (factory_name, NULL);
  if (test_el == NULL)
    return FALSE;

  GstBus *tmp_bus = gst_bus_new ();
  gst_element_set_bus (test_el, tmp_bus);

  if (strcmp (factory_name, "fakesink") == 0) {
    g_object_set (G_OBJECT (test_el), "sync", TRUE, NULL);
  }

  GstStateChangeReturn sret = gst_element_set_state (test_el, GST_STATE_READY);
  gboolean ok = (sret != GST_STATE_CHANGE_FAILURE);

  gst_bus_set_flushing (tmp_bus, TRUE);
  gst_element_set_state (test_el, GST_STATE_NULL);
  gst_element_set_bus (test_el, NULL);
  gst_object_unref (tmp_bus);
  gst_object_unref (test_el);
  return ok;
}

static void
olaz_set_ts_offset_if_supported (GstElement *el, gint64 ts_offset)
{
  if (el == NULL)
    return;
  GObjectClass *klass = G_OBJECT_GET_CLASS (el);
  if (klass != NULL && g_object_class_find_property (klass, "ts-offset") != NULL) {
    g_object_set (G_OBJECT (el), "ts-offset", ts_offset, NULL);
  }
}

/* ============================================================================
 * 1. `OlazAutoAudioSink` (`autoaudiosink`)
 * ============================================================================ */

#define OLAZ_TYPE_AUTO_AUDIO_SINK (olaz_auto_audio_sink_get_type ())
G_DECLARE_FINAL_TYPE (OlazAutoAudioSink, olaz_auto_audio_sink, OLAZ, AUTO_AUDIO_SINK, GstBin)

struct _OlazAutoAudioSink
{
  GstBin parent;
  GstPad *sinkpad;
  GstElement *volume_el;
  GstElement *child_sink;
  gdouble volume;
  gboolean mute;
  gint64 ts_offset;
};

enum
{
  AUDIO_PROP_0,
  AUDIO_PROP_VOLUME,
  AUDIO_PROP_MUTE,
  AUDIO_PROP_TS_OFFSET,
};

static GstStaticPadTemplate olaz_auto_audio_sink_template =
GST_STATIC_PAD_TEMPLATE ("sink",
    GST_PAD_SINK,
    GST_PAD_ALWAYS,
    GST_STATIC_CAPS ("audio/x-raw"));

static void
olaz_auto_audio_sink_stream_volume_init (GstStreamVolumeInterface *iface)
{
  (void) iface;
}

G_DEFINE_TYPE_WITH_CODE (OlazAutoAudioSink, olaz_auto_audio_sink, GST_TYPE_BIN,
    G_IMPLEMENT_INTERFACE (GST_TYPE_STREAM_VOLUME,
        olaz_auto_audio_sink_stream_volume_init))

static void
olaz_auto_audio_sink_apply_volume (OlazAutoAudioSink *self)
{
  if (self->volume_el != NULL) {
    g_object_set (G_OBJECT (self->volume_el),
        "volume", self->volume,
        "mute", self->mute,
        NULL);
  } else if (self->child_sink != NULL && GST_IS_STREAM_VOLUME (self->child_sink)) {
    gst_stream_volume_set_volume (GST_STREAM_VOLUME (self->child_sink),
        GST_STREAM_VOLUME_FORMAT_LINEAR, self->volume);
    gst_stream_volume_set_mute (GST_STREAM_VOLUME (self->child_sink), self->mute);
  }
}

static void
olaz_auto_audio_sink_teardown_children (OlazAutoAudioSink *self)
{
  if (self->sinkpad != NULL) {
    gst_ghost_pad_set_target (GST_GHOST_PAD (self->sinkpad), NULL);
  }
  if (self->volume_el != NULL) {
    gst_element_set_state (self->volume_el, GST_STATE_NULL);
    gst_bin_remove (GST_BIN (self), self->volume_el);
    self->volume_el = NULL;
  }
  if (self->child_sink != NULL) {
    gst_element_set_state (self->child_sink, GST_STATE_NULL);
    gst_bin_remove (GST_BIN (self), self->child_sink);
    self->child_sink = NULL;
  }
}

static gboolean
olaz_auto_audio_sink_setup_candidate (OlazAutoAudioSink *self, const char *factory_name)
{
  GstElement *sink = gst_element_factory_make (factory_name, "actual-audio-sink");
  if (sink == NULL)
    return FALSE;

  if (strcmp (factory_name, "fakesink") == 0) {
    g_object_set (G_OBJECT (sink), "sync", TRUE, NULL);
  }
  olaz_set_ts_offset_if_supported (sink, self->ts_offset);

  /* Add `sink` in GST_STATE_NULL first so WebKit's `child-added` callback can set
   * `client-name` and `stream-properties` (`media.role`) before READY connects to PipeWire. */
  if (!gst_bin_add (GST_BIN (self), sink)) {
    gst_object_unref (sink);
    return FALSE;
  }
  self->child_sink = sink;

  GstElement *head = sink;
  if (!GST_IS_STREAM_VOLUME (sink)) {
    GstElement *vol = gst_element_factory_make ("volume", "olaz-sink-volume");
    if (vol != NULL) {
      if (gst_bin_add (GST_BIN (self), vol)) {
        if (gst_element_link (vol, sink)) {
          self->volume_el = vol;
          head = vol;
        } else {
          gst_bin_remove (GST_BIN (self), vol);
        }
      } else {
        gst_object_unref (vol);
      }
    }
  }

  olaz_auto_audio_sink_apply_volume (self);

  GstPad *target_pad = gst_element_get_static_pad (head, "sink");
  if (target_pad == NULL) {
    olaz_auto_audio_sink_teardown_children (self);
    return FALSE;
  }
  gboolean ok = gst_ghost_pad_set_target (GST_GHOST_PAD (self->sinkpad), target_pad);
  gst_object_unref (target_pad);
  if (!ok) {
    olaz_auto_audio_sink_teardown_children (self);
    return FALSE;
  }

  return TRUE;
}

static GstStateChangeReturn
olaz_auto_audio_sink_change_state (GstElement *element, GstStateChange transition)
{
  OlazAutoAudioSink *self = OLAZ_AUTO_AUDIO_SINK (element);

  if (transition == GST_STATE_CHANGE_NULL_TO_READY && self->child_sink == NULL) {
    static const char *const kAudioCandidates[] = {
      "pipewiresink",
      "pulsesink",
      "alsasink",
      "fakesink",
    };

    const char *chosen = NULL;
    const char *override_sink = g_getenv ("OLAZ_GST_AUDIO_SINK");
    if (override_sink != NULL && override_sink[0] != '\0') {
      if (olaz_probe_sink_factory (override_sink))
        chosen = override_sink;
    }

    if (chosen == NULL) {
      for (gsize i = 0; i < G_N_ELEMENTS (kAudioCandidates); i++) {
        if (olaz_probe_sink_factory (kAudioCandidates[i])) {
          chosen = kAudioCandidates[i];
          break;
        }
      }
    }

    if (chosen == NULL || !olaz_auto_audio_sink_setup_candidate (self, chosen)) {
      return GST_STATE_CHANGE_FAILURE;
    }

    GstStateChangeReturn ret =
        GST_ELEMENT_CLASS (olaz_auto_audio_sink_parent_class)->change_state (element, transition);
    if (ret == GST_STATE_CHANGE_FAILURE && strcmp (chosen, "fakesink") != 0) {
      olaz_auto_audio_sink_teardown_children (self);
      if (!olaz_auto_audio_sink_setup_candidate (self, "fakesink"))
        return GST_STATE_CHANGE_FAILURE;
      ret = GST_ELEMENT_CLASS (olaz_auto_audio_sink_parent_class)->change_state (element, transition);
    }
    return ret;
  }

  GstStateChangeReturn ret =
      GST_ELEMENT_CLASS (olaz_auto_audio_sink_parent_class)->change_state (element, transition);

  if (transition == GST_STATE_CHANGE_READY_TO_NULL) {
    olaz_auto_audio_sink_teardown_children (self);
  }

  return ret;
}

static void
olaz_auto_audio_sink_set_property (GObject *object, guint prop_id,
    const GValue *value, GParamSpec *pspec)
{
  OlazAutoAudioSink *self = OLAZ_AUTO_AUDIO_SINK (object);
  switch (prop_id) {
    case AUDIO_PROP_VOLUME:
      self->volume = g_value_get_double (value);
      olaz_auto_audio_sink_apply_volume (self);
      break;
    case AUDIO_PROP_MUTE:
      self->mute = g_value_get_boolean (value);
      olaz_auto_audio_sink_apply_volume (self);
      break;
    case AUDIO_PROP_TS_OFFSET:
      self->ts_offset = g_value_get_int64 (value);
      olaz_set_ts_offset_if_supported (self->child_sink, self->ts_offset);
      break;
    default:
      G_OBJECT_WARN_INVALID_PROPERTY_ID (object, prop_id, pspec);
      break;
  }
}

static void
olaz_auto_audio_sink_get_property (GObject *object, guint prop_id,
    GValue *value, GParamSpec *pspec)
{
  OlazAutoAudioSink *self = OLAZ_AUTO_AUDIO_SINK (object);
  switch (prop_id) {
    case AUDIO_PROP_VOLUME:
      g_value_set_double (value, self->volume);
      break;
    case AUDIO_PROP_MUTE:
      g_value_set_boolean (value, self->mute);
      break;
    case AUDIO_PROP_TS_OFFSET:
      g_value_set_int64 (value, self->ts_offset);
      break;
    default:
      G_OBJECT_WARN_INVALID_PROPERTY_ID (object, prop_id, pspec);
      break;
  }
}

static void
olaz_auto_audio_sink_init (OlazAutoAudioSink *self)
{
  olaz_ensure_dynamic_webkit_media_src ();

  self->volume = 1.0;
  self->mute = FALSE;
  self->ts_offset = 0;

  GST_OBJECT_FLAG_SET (self, GST_ELEMENT_FLAG_SINK);
  gst_bin_set_suppressed_flags (GST_BIN (self),
      (GstElementFlags) (GST_ELEMENT_FLAG_SOURCE | GST_ELEMENT_FLAG_SINK));

  GstPadTemplate *tmpl = gst_static_pad_template_get (&olaz_auto_audio_sink_template);
  self->sinkpad = gst_ghost_pad_new_no_target_from_template ("sink", tmpl);
  gst_object_unref (tmpl);
  gst_element_add_pad (GST_ELEMENT (self), self->sinkpad);
}

static void
olaz_auto_audio_sink_class_init (OlazAutoAudioSinkClass *klass)
{
  GObjectClass *gobject_class = G_OBJECT_CLASS (klass);
  GstElementClass *element_class = GST_ELEMENT_CLASS (klass);

  gobject_class->set_property = olaz_auto_audio_sink_set_property;
  gobject_class->get_property = olaz_auto_audio_sink_get_property;

  g_object_class_install_property (gobject_class, AUDIO_PROP_VOLUME,
      g_param_spec_double ("volume", "Volume", "Linear audio volume factor",
          0.0, 10.0, 1.0, G_PARAM_READWRITE | G_PARAM_STATIC_STRINGS));
  g_object_class_install_property (gobject_class, AUDIO_PROP_MUTE,
      g_param_spec_boolean ("mute", "Mute", "Mute audio output",
          FALSE, G_PARAM_READWRITE | G_PARAM_STATIC_STRINGS));
  g_object_class_install_property (gobject_class, AUDIO_PROP_TS_OFFSET,
      g_param_spec_int64 ("ts-offset", "Timestamp Offset",
          "Timestamp offset in nanoseconds",
          G_MININT64, G_MAXINT64, 0, G_PARAM_READWRITE | G_PARAM_STATIC_STRINGS));

  gst_element_class_set_static_metadata (element_class,
      "Olaz Auto Audio Sink",
      "Sink/Audio",
      "Automatically selects PipeWire, PulseAudio, ALSA, or synced fake audio sink",
      "Olaz Contributors");
  gst_element_class_add_static_pad_template (element_class, &olaz_auto_audio_sink_template);

  element_class->change_state = olaz_auto_audio_sink_change_state;
}

/* ============================================================================
 * 2. `OlazAutoVideoSink` (`autovideosink`)
 * ============================================================================ */

#define OLAZ_TYPE_AUTO_VIDEO_SINK (olaz_auto_video_sink_get_type ())
G_DECLARE_FINAL_TYPE (OlazAutoVideoSink, olaz_auto_video_sink, OLAZ, AUTO_VIDEO_SINK, GstBin)

struct _OlazAutoVideoSink
{
  GstBin parent;
  GstPad *sinkpad;
  GstElement *child_sink;
  gint64 ts_offset;
};

enum
{
  VIDEO_PROP_0,
  VIDEO_PROP_TS_OFFSET,
};

static GstStaticPadTemplate olaz_auto_video_sink_template =
GST_STATIC_PAD_TEMPLATE ("sink",
    GST_PAD_SINK,
    GST_PAD_ALWAYS,
    GST_STATIC_CAPS ("video/x-raw"));

G_DEFINE_TYPE (OlazAutoVideoSink, olaz_auto_video_sink, GST_TYPE_BIN)

static void
olaz_auto_video_sink_teardown (OlazAutoVideoSink *self)
{
  if (self->sinkpad != NULL) {
    gst_ghost_pad_set_target (GST_GHOST_PAD (self->sinkpad), NULL);
  }
  if (self->child_sink != NULL) {
    gst_element_set_state (self->child_sink, GST_STATE_NULL);
    gst_bin_remove (GST_BIN (self), self->child_sink);
    self->child_sink = NULL;
  }
}

static gboolean
olaz_auto_video_sink_setup_candidate (OlazAutoVideoSink *self, const char *factory_name)
{
  GstElement *sink = gst_element_factory_make (factory_name, "actual-video-sink");
  if (sink == NULL)
    return FALSE;

  if (strcmp (factory_name, "fakesink") == 0) {
    g_object_set (G_OBJECT (sink), "sync", TRUE, NULL);
  }
  olaz_set_ts_offset_if_supported (sink, self->ts_offset);

  if (!gst_bin_add (GST_BIN (self), sink)) {
    gst_object_unref (sink);
    return FALSE;
  }
  self->child_sink = sink;

  GstPad *target_pad = gst_element_get_static_pad (sink, "sink");
  if (target_pad == NULL) {
    olaz_auto_video_sink_teardown (self);
    return FALSE;
  }
  gboolean ok = gst_ghost_pad_set_target (GST_GHOST_PAD (self->sinkpad), target_pad);
  gst_object_unref (target_pad);
  if (!ok) {
    olaz_auto_video_sink_teardown (self);
    return FALSE;
  }
  return TRUE;
}

static GstStateChangeReturn
olaz_auto_video_sink_change_state (GstElement *element, GstStateChange transition)
{
  OlazAutoVideoSink *self = OLAZ_AUTO_VIDEO_SINK (element);

  if (transition == GST_STATE_CHANGE_NULL_TO_READY && self->child_sink == NULL) {
    static const char *const kVideoCandidates[] = {
      "waylandsink",
      "glimagesink",
      "xvimagesink",
      "ximagesink",
      "fakesink",
    };

    const char *chosen = NULL;
    for (gsize i = 0; i < G_N_ELEMENTS (kVideoCandidates); i++) {
      if (olaz_probe_sink_factory (kVideoCandidates[i])) {
        chosen = kVideoCandidates[i];
        break;
      }
    }

    if (chosen == NULL || !olaz_auto_video_sink_setup_candidate (self, chosen))
      return GST_STATE_CHANGE_FAILURE;

    GstStateChangeReturn ret =
        GST_ELEMENT_CLASS (olaz_auto_video_sink_parent_class)->change_state (element, transition);
    if (ret == GST_STATE_CHANGE_FAILURE && strcmp (chosen, "fakesink") != 0) {
      olaz_auto_video_sink_teardown (self);
      if (!olaz_auto_video_sink_setup_candidate (self, "fakesink"))
        return GST_STATE_CHANGE_FAILURE;
      ret = GST_ELEMENT_CLASS (olaz_auto_video_sink_parent_class)->change_state (element, transition);
    }
    return ret;
  }

  GstStateChangeReturn ret =
      GST_ELEMENT_CLASS (olaz_auto_video_sink_parent_class)->change_state (element, transition);

  if (transition == GST_STATE_CHANGE_READY_TO_NULL) {
    olaz_auto_video_sink_teardown (self);
  }
  return ret;
}

static void
olaz_auto_video_sink_set_property (GObject *object, guint prop_id,
    const GValue *value, GParamSpec *pspec)
{
  OlazAutoVideoSink *self = OLAZ_AUTO_VIDEO_SINK (object);
  switch (prop_id) {
    case VIDEO_PROP_TS_OFFSET:
      self->ts_offset = g_value_get_int64 (value);
      olaz_set_ts_offset_if_supported (self->child_sink, self->ts_offset);
      break;
    default:
      G_OBJECT_WARN_INVALID_PROPERTY_ID (object, prop_id, pspec);
      break;
  }
}

static void
olaz_auto_video_sink_get_property (GObject *object, guint prop_id,
    GValue *value, GParamSpec *pspec)
{
  OlazAutoVideoSink *self = OLAZ_AUTO_VIDEO_SINK (object);
  switch (prop_id) {
    case VIDEO_PROP_TS_OFFSET:
      g_value_set_int64 (value, self->ts_offset);
      break;
    default:
      G_OBJECT_WARN_INVALID_PROPERTY_ID (object, prop_id, pspec);
      break;
  }
}

static void
olaz_auto_video_sink_init (OlazAutoVideoSink *self)
{
  olaz_ensure_dynamic_webkit_media_src ();

  self->ts_offset = 0;
  GST_OBJECT_FLAG_SET (self, GST_ELEMENT_FLAG_SINK);
  gst_bin_set_suppressed_flags (GST_BIN (self),
      (GstElementFlags) (GST_ELEMENT_FLAG_SOURCE | GST_ELEMENT_FLAG_SINK));

  GstPadTemplate *tmpl = gst_static_pad_template_get (&olaz_auto_video_sink_template);
  self->sinkpad = gst_ghost_pad_new_no_target_from_template ("sink", tmpl);
  gst_object_unref (tmpl);
  gst_element_add_pad (GST_ELEMENT (self), self->sinkpad);
}

static void
olaz_auto_video_sink_class_init (OlazAutoVideoSinkClass *klass)
{
  GObjectClass *gobject_class = G_OBJECT_CLASS (klass);
  GstElementClass *element_class = GST_ELEMENT_CLASS (klass);

  gobject_class->set_property = olaz_auto_video_sink_set_property;
  gobject_class->get_property = olaz_auto_video_sink_get_property;

  g_object_class_install_property (gobject_class, VIDEO_PROP_TS_OFFSET,
      g_param_spec_int64 ("ts-offset", "Timestamp Offset",
          "Timestamp offset in nanoseconds",
          G_MININT64, G_MAXINT64, 0, G_PARAM_READWRITE | G_PARAM_STATIC_STRINGS));

  gst_element_class_set_static_metadata (element_class,
      "Olaz Auto Video Sink",
      "Sink/Video",
      "Automatically selects Wayland, GL, X11, or synced fake video sink",
      "Olaz Contributors");
  gst_element_class_add_static_pad_template (element_class, &olaz_auto_video_sink_template);

  element_class->change_state = olaz_auto_video_sink_change_state;
}

/* ============================================================================
 * 3. `OlazWavParse` (`olazwavparse`, rank `GST_RANK_MARGINAL`)
 * ============================================================================ */

#define OLAZ_TYPE_WAV_PARSE (olaz_wav_parse_get_type ())
G_DECLARE_FINAL_TYPE (OlazWavParse, olaz_wav_parse, OLAZ, WAV_PARSE, GstElement)

struct _OlazWavParse
{
  GstElement parent;
  GstPad *sinkpad;
  GstPad *srcpad;
  GstAdapter *adapter;

  gboolean sent_stream_start;
  gboolean riff_checked;
  gboolean got_fmt;
  gboolean got_data;
  gboolean need_segment;

  const char *gst_format;
  guint rate;
  guint channels;
  guint bpf;

  guint64 parse_offset;
  guint64 data_offset;
  guint64 data_size;      /* 0 if unbounded */
  guint64 bytes_emitted;
  guint64 samples_emitted;
  GstClockTime duration_ns;
  GstClockTime segment_start_ns;
};

static GstStaticPadTemplate olaz_wav_parse_sink_template =
GST_STATIC_PAD_TEMPLATE ("sink",
    GST_PAD_SINK,
    GST_PAD_ALWAYS,
    GST_STATIC_CAPS ("audio/x-wav"));

static GstStaticPadTemplate olaz_wav_parse_src_template =
GST_STATIC_PAD_TEMPLATE ("src",
    GST_PAD_SRC,
    GST_PAD_ALWAYS,
    GST_STATIC_CAPS ("audio/x-raw, "
        "format = (string) { U8, S16LE, S24LE, S32LE, F32LE, F64LE }, "
        "layout = (string) interleaved, "
        "rate = (int) [ 1, 384000 ], "
        "channels = (int) [ 1, 8 ]"));

G_DEFINE_TYPE (OlazWavParse, olaz_wav_parse, GST_TYPE_ELEMENT)

static void
olaz_wav_parse_reset (OlazWavParse *self)
{
  if (self->adapter != NULL)
    gst_adapter_clear (self->adapter);
  self->sent_stream_start = FALSE;
  self->riff_checked = FALSE;
  self->got_fmt = FALSE;
  self->got_data = FALSE;
  self->need_segment = FALSE;
  self->gst_format = NULL;
  self->rate = 0;
  self->channels = 0;
  self->bpf = 0;
  self->parse_offset = 0;
  self->data_offset = 0;
  self->data_size = 0;
  self->bytes_emitted = 0;
  self->samples_emitted = 0;
  self->duration_ns = GST_CLOCK_TIME_NONE;
  self->segment_start_ns = 0;
}

static const char *
olaz_wav_resolve_pcm_format (guint16 format_tag, guint16 bits_per_sample)
{
  if (format_tag == 1) { /* WAVE_FORMAT_PCM */
    switch (bits_per_sample) {
      case 8:  return "U8";
      case 16: return "S16LE";
      case 24: return "S24LE";
      case 32: return "S32LE";
      default: return NULL;
    }
  } else if (format_tag == 3) { /* WAVE_FORMAT_IEEE_FLOAT */
    switch (bits_per_sample) {
      case 32: return "F32LE";
      case 64: return "F64LE";
      default: return NULL;
    }
  }
  return NULL;
}

static void
olaz_wav_parse_push_segment (OlazWavParse *self)
{
  GstSegment seg;
  gst_segment_init (&seg, GST_FORMAT_TIME);
  seg.start = self->segment_start_ns;
  seg.time = self->segment_start_ns;
  seg.position = self->segment_start_ns;
  seg.duration = self->duration_ns;
  gst_pad_push_event (self->srcpad, gst_event_new_segment (&seg));
  self->need_segment = FALSE;
}

static GstFlowReturn
olaz_wav_parse_drain_samples (OlazWavParse *self, gboolean flush_remainder)
{
  if (!self->got_data || self->bpf == 0 || self->rate == 0)
    return GST_FLOW_OK;

  if (self->need_segment) {
    olaz_wav_parse_push_segment (self);
  }

  gsize chunk_frames = MAX (self->rate / 50, 256u);
  gsize target_chunk_bytes = chunk_frames * self->bpf;

  while (TRUE) {
    gsize avail = gst_adapter_available (self->adapter);
    if (self->data_size > 0) {
      if (self->bytes_emitted >= self->data_size) {
        if (avail > 0)
          gst_adapter_flush (self->adapter, avail);
        return GST_FLOW_EOS;
      }
      guint64 rem = self->data_size - self->bytes_emitted;
      if ((guint64) avail > rem)
        avail = (gsize) rem;
    }

    gsize aligned = avail - (avail % self->bpf);
    if (aligned == 0)
      break;
    if (!flush_remainder && aligned < target_chunk_bytes &&
        (self->data_size == 0 || self->bytes_emitted + aligned < self->data_size)) {
      break;
    }

    gsize take = MIN (aligned, target_chunk_bytes);
    guint64 frames = take / self->bpf;
    GstBuffer *buf = gst_adapter_take_buffer (self->adapter, take);
    if (buf == NULL)
      break;

    buf = gst_buffer_make_writable (buf);
    GST_BUFFER_PTS (buf) = gst_util_uint64_scale_int (self->samples_emitted, GST_SECOND, self->rate);
    GST_BUFFER_DTS (buf) = GST_BUFFER_PTS (buf);
    GST_BUFFER_DURATION (buf) = gst_util_uint64_scale_int (frames, GST_SECOND, self->rate);
    GST_BUFFER_OFFSET (buf) = self->samples_emitted;
    GST_BUFFER_OFFSET_END (buf) = self->samples_emitted + frames;

    self->bytes_emitted += take;
    self->samples_emitted += frames;

    GstFlowReturn ret = gst_pad_push (self->srcpad, buf);
    if (ret != GST_FLOW_OK)
      return ret;
  }

  return GST_FLOW_OK;
}

static GstFlowReturn
olaz_wav_parse_chain (GstPad *pad, GstObject *parent, GstBuffer *buf)
{
  (void) pad;
  OlazWavParse *self = OLAZ_WAV_PARSE (parent);

  gst_adapter_push (self->adapter, buf);

  if (!self->riff_checked) {
    if (gst_adapter_available (self->adapter) < 12)
      return GST_FLOW_OK;
    const guint8 *hdr = gst_adapter_map (self->adapter, 12);
    if (hdr == NULL || memcmp (hdr, "RIFF", 4) != 0 || memcmp (hdr + 8, "WAVE", 4) != 0) {
      gst_adapter_unmap (self->adapter);
      GST_ELEMENT_ERROR (self, STREAM, WRONG_TYPE, ("Not a RIFF/WAVE stream"), (NULL));
      return GST_FLOW_ERROR;
    }
    gst_adapter_unmap (self->adapter);
    gst_adapter_flush (self->adapter, 12);
    self->parse_offset = 12;
    self->riff_checked = TRUE;
  }

  while (!self->got_data) {
    if (gst_adapter_available (self->adapter) < 8)
      return GST_FLOW_OK;

    const guint8 *chdr = gst_adapter_map (self->adapter, 8);
    if (chdr == NULL)
      return GST_FLOW_ERROR;
    char cid[4];
    memcpy (cid, chdr, 4);
    guint32 csize = GST_READ_UINT32_LE (chdr + 4);
    gst_adapter_unmap (self->adapter);

    if (memcmp (cid, "data", 4) == 0) {
      if (!self->got_fmt || self->gst_format == NULL || self->bpf == 0 || self->rate == 0) {
        GST_ELEMENT_ERROR (self, STREAM, DECODE, ("WAVE data chunk before fmt chunk"), (NULL));
        return GST_FLOW_ERROR;
      }
      gst_adapter_flush (self->adapter, 8);
      self->parse_offset += 8;
      self->data_offset = self->parse_offset;
      self->data_size = (csize == 0 || csize == 0xFFFFFFFFu) ? 0 : (guint64) csize;
      if (self->data_size > 0) {
        self->duration_ns = gst_util_uint64_scale_int (
            self->data_size / self->bpf, GST_SECOND, self->rate);
      } else {
        self->duration_ns = GST_CLOCK_TIME_NONE;
      }

      if (!self->sent_stream_start) {
        char *stream_id = gst_pad_create_stream_id (self->srcpad, GST_ELEMENT_CAST (self), "wav");
        gst_pad_push_event (self->srcpad, gst_event_new_stream_start (stream_id));
        g_free (stream_id);
        self->sent_stream_start = TRUE;
      }

      GstCaps *caps = gst_caps_new_simple ("audio/x-raw",
          "format", G_TYPE_STRING, self->gst_format,
          "layout", G_TYPE_STRING, "interleaved",
          "rate", G_TYPE_INT, (gint) self->rate,
          "channels", G_TYPE_INT, (gint) self->channels,
          NULL);
      if (self->channels >= 2) {
        guint64 mask = gst_audio_channel_get_fallback_mask (self->channels);
        if (mask != 0) {
          gst_caps_set_simple (caps, "channel-mask", GST_TYPE_BITMASK, mask, NULL);
        }
      }
      gst_pad_set_caps (self->srcpad, caps);
      gst_caps_unref (caps);

      self->got_data = TRUE;
      self->need_segment = TRUE;
      break;
    }

    gsize padded = (gsize) csize + ((csize & 1u) ? 1u : 0u);
    if (padded > 16u * 1024u * 1024u) {
      GST_ELEMENT_ERROR (self, STREAM, DECODE, ("Oversized RIFF metadata chunk"), (NULL));
      return GST_FLOW_ERROR;
    }
    if (gst_adapter_available (self->adapter) < 8 + padded)
      return GST_FLOW_OK;

    if (memcmp (cid, "fmt ", 4) == 0) {
      if (csize < 16) {
        GST_ELEMENT_ERROR (self, STREAM, DECODE, ("Short WAVE fmt chunk"), (NULL));
        return GST_FLOW_ERROR;
      }
      const guint8 *fmt = gst_adapter_map (self->adapter, 8 + csize);
      guint16 format_tag = GST_READ_UINT16_LE (fmt + 8);
      guint16 channels = GST_READ_UINT16_LE (fmt + 10);
      guint32 rate = GST_READ_UINT32_LE (fmt + 12);
      guint16 bits = GST_READ_UINT16_LE (fmt + 22);
      if (format_tag == 0xFFFEu && csize >= 40) {
        format_tag = GST_READ_UINT16_LE (fmt + 32);
      }
      gst_adapter_unmap (self->adapter);

      const char *fmt_str = olaz_wav_resolve_pcm_format (format_tag, bits);
      if (fmt_str == NULL || channels < 1 || channels > 8 || rate < 1 || rate > 384000) {
        GST_ELEMENT_ERROR (self, STREAM, NOT_IMPLEMENTED,
            ("Unsupported WAVE format tag=%u bits=%u channels=%u rate=%u",
             format_tag, bits, channels, rate), (NULL));
        return GST_FLOW_NOT_SUPPORTED;
      }

      self->gst_format = fmt_str;
      self->channels = channels;
      self->rate = rate;
      self->bpf = (guint) channels * ((guint) bits / 8u);
      self->got_fmt = TRUE;
    }

    gst_adapter_flush (self->adapter, 8 + padded);
    self->parse_offset += 8 + padded;
  }

  return olaz_wav_parse_drain_samples (self, FALSE);
}

static gboolean
olaz_wav_parse_sink_event (GstPad *pad, GstObject *parent, GstEvent *event)
{
  OlazWavParse *self = OLAZ_WAV_PARSE (parent);

  switch (GST_EVENT_TYPE (event)) {
    case GST_EVENT_STREAM_START:
      gst_event_unref (event);
      return TRUE;

    case GST_EVENT_SEGMENT: {
      const GstSegment *in_seg = NULL;
      gst_event_parse_segment (event, &in_seg);
      if (self->got_data && in_seg != NULL && in_seg->format == GST_FORMAT_BYTES) {
        guint64 byte_start = (in_seg->start > self->data_offset)
            ? (in_seg->start - self->data_offset)
            : 0;
        byte_start -= (byte_start % self->bpf);
        self->bytes_emitted = byte_start;
        self->samples_emitted = byte_start / self->bpf;
        self->segment_start_ns =
            gst_util_uint64_scale_int (self->samples_emitted, GST_SECOND, self->rate);
        self->need_segment = TRUE;
      }
      gst_event_unref (event);
      return TRUE;
    }

    case GST_EVENT_FLUSH_STOP:
      gst_adapter_clear (self->adapter);
      return gst_pad_event_default (pad, parent, event);

    case GST_EVENT_EOS:
      olaz_wav_parse_drain_samples (self, TRUE);
      gst_adapter_clear (self->adapter);
      return gst_pad_push_event (self->srcpad, event);

    default:
      return gst_pad_event_default (pad, parent, event);
  }
}

static gboolean
olaz_wav_parse_src_query (GstPad *pad, GstObject *parent, GstQuery *query)
{
  OlazWavParse *self = OLAZ_WAV_PARSE (parent);

  switch (GST_QUERY_TYPE (query)) {
    case GST_QUERY_DURATION: {
      GstFormat fmt;
      gst_query_parse_duration (query, &fmt, NULL);
      if (fmt == GST_FORMAT_TIME && self->got_data && GST_CLOCK_TIME_IS_VALID (self->duration_ns)) {
        gst_query_set_duration (query, GST_FORMAT_TIME, (gint64) self->duration_ns);
        return TRUE;
      }
      return FALSE;
    }

    case GST_QUERY_SEEKING: {
      GstFormat fmt;
      gst_query_parse_seeking (query, &fmt, NULL, NULL, NULL);
      if (fmt == GST_FORMAT_TIME && self->got_data) {
        GstQuery *bquery = gst_query_new_seeking (GST_FORMAT_BYTES);
        gboolean seekable = FALSE;
        if (gst_pad_peer_query (self->sinkpad, bquery)) {
          gst_query_parse_seeking (bquery, NULL, &seekable, NULL, NULL);
        }
        gst_query_unref (bquery);
        gint64 end = GST_CLOCK_TIME_IS_VALID (self->duration_ns)
            ? (gint64) self->duration_ns
            : -1;
        gst_query_set_seeking (query, GST_FORMAT_TIME, seekable, 0, end);
        return TRUE;
      }
      return FALSE;
    }

    default:
      return gst_pad_query_default (pad, parent, query);
  }
}

static gboolean
olaz_wav_parse_src_event (GstPad *pad, GstObject *parent, GstEvent *event)
{
  OlazWavParse *self = OLAZ_WAV_PARSE (parent);

  if (GST_EVENT_TYPE (event) == GST_EVENT_SEEK && self->got_data && self->bpf > 0 && self->rate > 0) {
    gdouble rate = 1.0;
    GstFormat fmt = GST_FORMAT_UNDEFINED;
    GstSeekFlags flags = GST_SEEK_FLAG_NONE;
    GstSeekType start_type = GST_SEEK_TYPE_NONE;
    gint64 start = 0;
    GstSeekType stop_type = GST_SEEK_TYPE_NONE;
    gint64 stop = -1;
    gst_event_parse_seek (event, &rate, &fmt, &flags, &start_type, &start, &stop_type, &stop);

    if (fmt == GST_FORMAT_TIME && start_type == GST_SEEK_TYPE_SET && start >= 0) {
      guint64 target_sample = gst_util_uint64_scale_int ((guint64) start, self->rate, GST_SECOND);
      guint64 target_byte = self->data_offset + target_sample * self->bpf;
      if (self->data_size > 0 && target_byte > self->data_offset + self->data_size) {
        target_byte = self->data_offset + self->data_size;
      }
      GstEvent *byte_seek = gst_event_new_seek (rate, GST_FORMAT_BYTES, flags,
          GST_SEEK_TYPE_SET, (gint64) target_byte, GST_SEEK_TYPE_NONE, -1);
      gst_event_unref (event);
      return gst_pad_push_event (self->sinkpad, byte_seek);
    }
  }

  return gst_pad_event_default (pad, parent, event);
}

static GstStateChangeReturn
olaz_wav_parse_change_state (GstElement *element, GstStateChange transition)
{
  OlazWavParse *self = OLAZ_WAV_PARSE (element);

  if (transition == GST_STATE_CHANGE_READY_TO_PAUSED) {
    olaz_wav_parse_reset (self);
  }

  GstStateChangeReturn ret =
      GST_ELEMENT_CLASS (olaz_wav_parse_parent_class)->change_state (element, transition);

  if (transition == GST_STATE_CHANGE_PAUSED_TO_READY) {
    olaz_wav_parse_reset (self);
  }
  return ret;
}

static void
olaz_wav_parse_finalize (GObject *object)
{
  OlazWavParse *self = OLAZ_WAV_PARSE (object);
  g_clear_object (&self->adapter);
  G_OBJECT_CLASS (olaz_wav_parse_parent_class)->finalize (object);
}

static void
olaz_wav_parse_init (OlazWavParse *self)
{
  self->adapter = gst_adapter_new ();

  self->sinkpad = gst_pad_new_from_static_template (&olaz_wav_parse_sink_template, "sink");
  gst_pad_set_chain_function (self->sinkpad, GST_DEBUG_FUNCPTR (olaz_wav_parse_chain));
  gst_pad_set_event_function (self->sinkpad, GST_DEBUG_FUNCPTR (olaz_wav_parse_sink_event));
  gst_element_add_pad (GST_ELEMENT (self), self->sinkpad);

  self->srcpad = gst_pad_new_from_static_template (&olaz_wav_parse_src_template, "src");
  gst_pad_use_fixed_caps (self->srcpad);
  gst_pad_set_query_function (self->srcpad, GST_DEBUG_FUNCPTR (olaz_wav_parse_src_query));
  gst_pad_set_event_function (self->srcpad, GST_DEBUG_FUNCPTR (olaz_wav_parse_src_event));
  gst_element_add_pad (GST_ELEMENT (self), self->srcpad);

  olaz_wav_parse_reset (self);
}

static void
olaz_wav_parse_class_init (OlazWavParseClass *klass)
{
  GObjectClass *gobject_class = G_OBJECT_CLASS (klass);
  GstElementClass *element_class = GST_ELEMENT_CLASS (klass);

  gobject_class->finalize = olaz_wav_parse_finalize;
  element_class->change_state = olaz_wav_parse_change_state;

  gst_element_class_set_static_metadata (element_class,
      "Olaz WAV Audio Demuxer",
      "Codec/Demuxer/Audio",
      "Parses RIFF/WAVE audio streams into raw PCM/float samples",
      "Olaz Contributors");
  gst_element_class_add_static_pad_template (element_class, &olaz_wav_parse_sink_template);
  gst_element_class_add_static_pad_template (element_class, &olaz_wav_parse_src_template);
}

/* ============================================================================
 * 4. `OlazWebKitMediaSrc` (`olazmediasrc` — Custom URI Scheme -> `WebKitWebSrc`)
 *
 * WebKit2GTK's internal `webkitwebsrc` (`WebKitWebSrc`) already routes any
 * hierarchical custom URI scheme registered via `webkit_web_context_register_uri_scheme`
 * (including `Range` / HTTP 206 byte-range requests) through `PlatformMediaResourceLoader`,
 * and `GStreamerCommon::isProtocolAllowed` allows custom schemes listed in
 * `WEBKIT_GST_ALLOWED_URI_PROTOCOLS`. However, `webkitwebsrc`'s `GstURIHandler`
 * only advertises `{"http", "https", "blob"}` to `playbin` / `uridecodebin`.
 *
 * When running inside `WebKitWebProcess` (where `webkitwebsrc` is registered),
 * this element dynamically subclasses `WebKitWebSrc` (`WEBKIT_IS_WEB_SRC == TRUE`
 * for `MediaPlayerPrivateGStreamer::sourceSetup`) and advertises `"olaz-media"`
 * (plus any additional custom schemes in `WEBKIT_GST_ALLOWED_URI_PROTOCOLS`).
 * When scanned out-of-process by `gst-plugin-scanner` (where `webkitwebsrc` is
 * absent), it registers a lightweight `GstElement` `GstURIHandler` stub with the
 * exact same URI protocols so the GStreamer registry caches the `GST_URI_SRC`
 * protocol mapping for `"olaz-media"`.
 * ============================================================================ */

static GstStaticPadTemplate olaz_media_src_pad_template =
GST_STATIC_PAD_TEMPLATE ("src",
    GST_PAD_SRC,
    GST_PAD_ALWAYS,
    GST_STATIC_CAPS_ANY);

static gboolean
olaz_is_builtin_webkit_scheme (const char *scheme)
{
  static const char *const kBuiltins[] = {
    "http", "https", "blob", "data", "file", "mediastream", "mediasourceblob",
  };
  for (gsize i = 0; i < G_N_ELEMENTS (kBuiltins); i++) {
    if (g_ascii_strcasecmp (scheme, kBuiltins[i]) == 0)
      return TRUE;
  }
  return FALSE;
}

static gpointer
olaz_build_custom_protocols_once (gpointer unused)
{
  (void) unused;
  GPtrArray *arr = g_ptr_array_new ();
  g_ptr_array_add (arr, g_strdup ("olaz-media"));
  g_ptr_array_add (arr, g_strdup ("zfile"));
  g_ptr_array_add (arr, g_strdup ("zalo-app"));

  const char *allowed_env = g_getenv ("WEBKIT_GST_ALLOWED_URI_PROTOCOLS");
  if (allowed_env != NULL && allowed_env[0] != '\0') {
    char **parts = g_strsplit (allowed_env, ",", -1);
    for (gsize i = 0; parts != NULL && parts[i] != NULL; i++) {
      char *trimmed = g_strstrip (parts[i]);
      if (trimmed[0] == '\0' || olaz_is_builtin_webkit_scheme (trimmed))
        continue;
      char *lower = g_ascii_strdown (trimmed, -1);
      gboolean exists = FALSE;
      for (guint j = 0; j < arr->len; j++) {
        if (strcmp ((const char *) g_ptr_array_index (arr, j), lower) == 0) {
          exists = TRUE;
          break;
        }
      }
      if (!exists) {
        g_ptr_array_add (arr, lower);
      } else {
        g_free (lower);
      }
    }
    g_strfreev (parts);
  }

  g_ptr_array_add (arr, NULL);
  return g_ptr_array_free (arr, FALSE);
}

static char *
olaz_rewrite_custom_scheme_uri (const char *uri)
{
  if (uri == NULL)
    return NULL;
  const char *proxy_base = g_getenv ("OLAZ_GST_MEDIA_PROXY_BASE");
  const char *sep = strstr (uri, "://");
  if (proxy_base != NULL && proxy_base[0] != '\0' && sep != NULL && sep > uri) {
    char *scheme = g_strndup (uri, (gsize) (sep - uri));
    char *scheme_lower = g_ascii_strdown (scheme, -1);
    g_free (scheme);

    if (!olaz_is_builtin_webkit_scheme (scheme_lower)) {
      const char *rest = sep + 3;
      gsize base_len = strlen (proxy_base);
      while (base_len > 0 && proxy_base[base_len - 1] == '/')
        base_len--;
      char *trimmed_base = g_strndup (proxy_base, base_len);
      char *rewritten = g_strdup_printf ("%s/%s/%s", trimmed_base, scheme_lower, rest);
      g_free (trimmed_base);
      g_free (scheme_lower);
      return rewritten;
    }
    g_free (scheme_lower);
  }
  return g_strdup (uri);
}

static GstURIType
olaz_media_src_uri_get_type (GType type)
{
  (void) type;
  return GST_URI_SRC;
}

static const gchar *const *
olaz_media_src_uri_get_protocols (GType type)
{
  (void) type;
  static GOnce once = G_ONCE_INIT;
  g_once (&once, olaz_build_custom_protocols_once, NULL);
  return (const gchar *const *) once.retval;
}

static gchar *
olaz_media_src_stub_get_uri (GstURIHandler *handler)
{
  (void) handler;
  return NULL;
}

static gboolean
olaz_media_src_stub_set_uri (GstURIHandler *handler, const gchar *uri, GError **error)
{
  (void) handler;
  (void) uri;
  g_set_error_literal (error, GST_URI_ERROR, GST_URI_ERROR_UNSUPPORTED_PROTOCOL,
      "OlazMediaSrc requires WebKitWebSrc inside WebKitWebProcess");
  return FALSE;
}

static GstURIHandlerInterface *olaz_webkit_parent_uri_iface = NULL;

static gboolean
olaz_webkit_media_src_set_uri (GstURIHandler *handler, const gchar *uri, GError **error)
{
  if (olaz_webkit_parent_uri_iface == NULL || olaz_webkit_parent_uri_iface->set_uri == NULL) {
    return olaz_media_src_stub_set_uri (handler, uri, error);
  }

  if (uri == NULL) {
    return olaz_webkit_parent_uri_iface->set_uri (handler, NULL, error);
  }

  char *rewritten = olaz_rewrite_custom_scheme_uri (uri);
  gboolean ok = olaz_webkit_parent_uri_iface->set_uri (handler, rewritten, error);
  g_free (rewritten);
  return ok;
}

static void
olaz_webkit_media_src_uri_handler_init (gpointer g_iface, gpointer iface_data)
{
  (void) iface_data;
  GstURIHandlerInterface *iface = (GstURIHandlerInterface *) g_iface;
  GstURIHandlerInterface *parent_iface =
      (GstURIHandlerInterface *) g_type_interface_peek_parent (g_iface);

  iface->get_type = olaz_media_src_uri_get_type;
  iface->get_protocols = olaz_media_src_uri_get_protocols;
  if (parent_iface != NULL && parent_iface->get_uri != NULL && parent_iface->set_uri != NULL) {
    olaz_webkit_parent_uri_iface = parent_iface;
    iface->get_uri = parent_iface->get_uri;
    iface->set_uri = olaz_webkit_media_src_set_uri;
  } else {
    iface->get_uri = olaz_media_src_stub_get_uri;
    iface->set_uri = olaz_media_src_stub_set_uri;
  }
}

static void
olaz_webkit_media_src_class_init (gpointer g_class, gpointer class_data)
{
  (void) class_data;
  GstElementClass *element_class = GST_ELEMENT_CLASS (g_class);
  gst_element_class_set_static_metadata (element_class,
      "Olaz WebKit Custom Scheme Source",
      "Source/Network",
      "Routes custom WebKit URI schemes (olaz-media://, zfile://) through WebKitWebSrc",
      "Olaz Contributors");
  if (gst_element_class_get_pad_template (element_class, "src") == NULL) {
    gst_element_class_add_static_pad_template (element_class, &olaz_media_src_pad_template);
  }
}

static GType
olaz_webkit_media_src_get_or_register_subclass (void)
{
  static GMutex reg_lock;
  static GType registered_type = 0;

  g_mutex_lock (&reg_lock);
  if (registered_type == 0) {
    GstElementFactory *wf = gst_element_factory_find ("webkitwebsrc");
    if (wf != NULL) {
      GstPluginFeature *loaded = gst_plugin_feature_load (GST_PLUGIN_FEATURE (wf));
      if (loaded != NULL) {
        GType parent_type = gst_element_factory_get_element_type (GST_ELEMENT_FACTORY (loaded));
        if (parent_type != 0) {
          GTypeQuery query;
          g_type_query (parent_type, &query);
          if (query.type != 0 && query.class_size > 0 && query.instance_size > 0) {
            GTypeInfo info;
            memset (&info, 0, sizeof (info));
            info.class_size = (guint16) query.class_size;
            info.class_init = olaz_webkit_media_src_class_init;
            info.instance_size = (guint16) query.instance_size;

            registered_type = g_type_register_static (parent_type, "OlazWebKitMediaSrc", &info, 0);
            static const GInterfaceInfo uri_info = {
              olaz_webkit_media_src_uri_handler_init,
              NULL,
              NULL,
            };
            g_type_add_interface_static (registered_type, GST_TYPE_URI_HANDLER, &uri_info);
          }
        }
        gst_object_unref (loaded);
      }
      gst_object_unref (wf);
    }
  }
  GType out = registered_type;
  g_mutex_unlock (&reg_lock);
  return out;
}

/* `OlazMediaSrcBin`: registered at plugin-scan time (before WebKit registers `webkitwebsrc`
 * inside `WebCore::ensureGStreamerInitialized()`), then delegates dynamically to `webkitwebsrc`
 * at `set_uri()` time so even the very first `zfile://` / `olaz-media://` media load succeeds! */
typedef struct _OlazMediaSrcBin
{
  GstBin parent;
  GstPad *srcpad;
  GstElement *inner_src;
  char *uri;
} OlazMediaSrcBin;

typedef struct _OlazMediaSrcBinClass
{
  GstBinClass parent_class;
} OlazMediaSrcBinClass;

static gchar *
olaz_media_src_bin_get_uri (GstURIHandler *handler)
{
  OlazMediaSrcBin *self = (OlazMediaSrcBin *) handler;
  return g_strdup (self->uri);
}

static gboolean
olaz_media_src_bin_set_uri (GstURIHandler *handler, const gchar *uri, GError **error)
{
  OlazMediaSrcBin *self = (OlazMediaSrcBin *) handler;
  olaz_ensure_dynamic_webkit_media_src ();

  g_free (self->uri);
  self->uri = g_strdup (uri);
  if (uri == NULL)
    return TRUE;

  if (self->inner_src == NULL) {
    GstElement *ws = gst_element_factory_make ("webkitwebsrc", "olaz-inner-webkit-src");
    if (ws == NULL)
      return olaz_media_src_stub_set_uri (handler, uri, error);
    if (!gst_bin_add (GST_BIN (self), ws)) {
      gst_object_unref (ws);
      return olaz_media_src_stub_set_uri (handler, uri, error);
    }
    self->inner_src = ws;
    GstPad *inner_pad = gst_element_get_static_pad (ws, "src");
    if (inner_pad != NULL) {
      gst_ghost_pad_set_target (GST_GHOST_PAD (self->srcpad), inner_pad);
      gst_object_unref (inner_pad);
    }
  }

  if (!GST_IS_URI_HANDLER (self->inner_src))
    return olaz_media_src_stub_set_uri (handler, uri, error);

  char *rewritten = olaz_rewrite_custom_scheme_uri (uri);
  gboolean ok = gst_uri_handler_set_uri (GST_URI_HANDLER (self->inner_src), rewritten, error);
  g_free (rewritten);
  return ok;
}

static void
olaz_media_src_bin_uri_init (gpointer g_iface, gpointer iface_data)
{
  (void) iface_data;
  GstURIHandlerInterface *iface = (GstURIHandlerInterface *) g_iface;
  iface->get_type = olaz_media_src_uri_get_type;
  iface->get_protocols = olaz_media_src_uri_get_protocols;
  iface->get_uri = olaz_media_src_bin_get_uri;
  iface->set_uri = olaz_media_src_bin_set_uri;
}

static void
olaz_media_src_bin_finalize (GObject *object)
{
  OlazMediaSrcBin *self = (OlazMediaSrcBin *) object;
  g_free (self->uri);
  self->uri = NULL;
  GObjectClass *parent_class = G_OBJECT_CLASS (g_type_class_peek (GST_TYPE_BIN));
  if (parent_class != NULL && parent_class->finalize != NULL)
    parent_class->finalize (object);
}

static void
olaz_media_src_bin_init (GTypeInstance *instance, gpointer g_class)
{
  (void) g_class;
  OlazMediaSrcBin *self = (OlazMediaSrcBin *) instance;
  self->srcpad = gst_ghost_pad_new_no_target_from_template ("src",
      gst_static_pad_template_get (&olaz_media_src_pad_template));
  gst_element_add_pad (GST_ELEMENT (self), self->srcpad);
}

static void
olaz_media_src_bin_class_init (gpointer g_class, gpointer class_data)
{
  olaz_webkit_media_src_class_init (g_class, class_data);
  GObjectClass *gobject_class = G_OBJECT_CLASS (g_class);
  gobject_class->finalize = olaz_media_src_bin_finalize;
}

static GType
olaz_media_src_stub_get_type (void)
{
  static gsize stub_type = 0;
  if (g_once_init_enter (&stub_type)) {
    GTypeInfo info;
    memset (&info, 0, sizeof (info));
    info.class_size = sizeof (OlazMediaSrcBinClass);
    info.class_init = olaz_media_src_bin_class_init;
    info.instance_size = sizeof (OlazMediaSrcBin);
    info.instance_init = olaz_media_src_bin_init;

    GType result = g_type_register_static (GST_TYPE_BIN, "OlazMediaSrcStub", &info, 0);
    static const GInterfaceInfo uri_info = {
      olaz_media_src_bin_uri_init,
      NULL,
      NULL,
    };
    g_type_add_interface_static (result, GST_TYPE_URI_HANDLER, &uri_info);
    g_once_init_leave (&stub_type, (gsize) result);
  }
  return (GType) stub_type;
}

static void
olaz_ensure_dynamic_webkit_media_src (void)
{
  static gint dynamic_registered = 0;
  if (g_atomic_int_get (&dynamic_registered))
    return;
  if (gst_element_factory_find ("webkitwebsrc") == NULL)
    return;
  GType sub_type = olaz_webkit_media_src_get_or_register_subclass ();
  if (sub_type != 0 && g_atomic_int_compare_and_exchange (&dynamic_registered, 0, 1)) {
    if (gst_element_factory_find ("olazwebkitmediasrc") == NULL) {
      gst_element_register (NULL, "olazwebkitmediasrc", GST_RANK_PRIMARY + 200, sub_type);
    }
  }
}

static gboolean
olaz_register_webkit_media_src_idle_cb (gpointer user_data)
{
  (void) user_data;
  olaz_ensure_dynamic_webkit_media_src ();
  return G_SOURCE_REMOVE;
}

/* ============================================================================
 * 5. `OlazAvDemux` (`olazavdemux` — FFmpeg libavformat + libavcodec fallback)
 *
 * Push-mode container + elementary stream demuxer & decoder backed by system
 * FFmpeg (`libavformat`, `libavcodec`, `libswresample`, `libswscale`).
 * Accumulates incoming push buffers into an anonymous `memfd` so both
 * streaming containers and `moov`-at-end mobile MP4/M4A files decode and seek
 * cleanly without `qtdemux`, `aacparse`, `mpegaudioparse`, or `gst-libav`.
 * ============================================================================ */

#ifdef OLAZ_HAVE_FFMPEG

#define OLAZ_TYPE_AV_DEMUX (olaz_av_demux_get_type ())
G_DECLARE_FINAL_TYPE (OlazAvDemux, olaz_av_demux, OLAZ, AV_DEMUX, GstElement)

struct _OlazAvDemux
{
  GstElement parent;
  GstPad *sinkpad;
  GstPad *audio_pad;
  GstPad *video_pad;

  GMutex io_lock;
  GCond io_cond;
  int memfd;
  guint64 write_pos;
  guint64 read_pos;
  gint64 total_size;
  gboolean queried_upstream_size;
  gboolean upstream_eos;
  gboolean stop_worker;
  gboolean worker_started;

  gint64 pending_seek_ns;
  gboolean pending_seek_flush;
  gdouble pending_seek_rate;
  guint32 last_seek_seqnum;
  GstClockTime duration_ns;

  GThread *worker_thread;
};

static GstStaticPadTemplate olaz_av_sink_full_template =
GST_STATIC_PAD_TEMPLATE ("sink",
    GST_PAD_SINK,
    GST_PAD_ALWAYS,
    GST_STATIC_CAPS (
        "video/quicktime; video/mp4; audio/x-m4a; application/x-3gp; "
        "video/webm; video/x-matroska; audio/webm; audio/x-matroska; "
        "application/ogg; video/ogg; audio/ogg; "
        "application/x-id3; audio/aac; audio/AMR; audio/AMR-WB; audio/x-flac; "
        "audio/mpeg"));

static GstStaticPadTemplate olaz_av_audio_src_template =
GST_STATIC_PAD_TEMPLATE ("audio_%u",
    GST_PAD_SRC,
    GST_PAD_SOMETIMES,
    GST_STATIC_CAPS (
        "audio/x-raw, "
        "format = (string) S16LE, "
        "layout = (string) interleaved, "
        "rate = (int) [ 8000, 192000 ], "
        "channels = (int) [ 1, 8 ]"));

static GstStaticPadTemplate olaz_av_video_src_template =
GST_STATIC_PAD_TEMPLATE ("video_%u",
    GST_PAD_SRC,
    GST_PAD_SOMETIMES,
    GST_STATIC_CAPS (
        "video/x-raw, "
        "format = (string) I420, "
        "width = (int) [ 1, 8192 ], "
        "height = (int) [ 1, 8192 ], "
        "framerate = (fraction) [ 0/1, 240/1 ]"));

G_DEFINE_TYPE (OlazAvDemux, olaz_av_demux, GST_TYPE_ELEMENT)

static int
olaz_avio_read (void *opaque, uint8_t *buf, int buf_size)
{
  OlazAvDemux *self = OLAZ_AV_DEMUX (opaque);
  if (buf_size <= 0)
    return AVERROR_EOF;

  g_mutex_lock (&self->io_lock);
  while (!self->stop_worker && !self->upstream_eos && self->read_pos >= self->write_pos) {
    g_cond_wait (&self->io_cond, &self->io_lock);
  }
  if (self->stop_worker) {
    g_mutex_unlock (&self->io_lock);
    return AVERROR_EXIT;
  }
  if (self->read_pos >= self->write_pos && self->upstream_eos) {
    g_mutex_unlock (&self->io_lock);
    return AVERROR_EOF;
  }

  guint64 avail = self->write_pos - self->read_pos;
  gsize to_read = MIN ((gsize) buf_size, (gsize) avail);
  int fd = self->memfd;
  guint64 off = self->read_pos;
  g_mutex_unlock (&self->io_lock);

  ssize_t n = pread (fd, buf, to_read, (off_t) off);
  if (n <= 0)
    return AVERROR_EOF;

  g_mutex_lock (&self->io_lock);
  self->read_pos += (guint64) n;
  g_mutex_unlock (&self->io_lock);
  return (int) n;
}

static int64_t
olaz_avio_seek (void *opaque, int64_t offset, int whence)
{
  OlazAvDemux *self = OLAZ_AV_DEMUX (opaque);

  g_mutex_lock (&self->io_lock);
  if (whence == AVSEEK_SIZE) {
    int64_t sz = -1;
    if (self->upstream_eos)
      sz = (int64_t) self->write_pos;
    else if (self->total_size > 0)
      sz = (int64_t) self->total_size;
    g_mutex_unlock (&self->io_lock);
    return sz;
  }

  whence &= ~AVSEEK_FORCE;
  int64_t target = -1;
  if (whence == SEEK_SET) {
    target = offset;
  } else if (whence == SEEK_CUR) {
    target = (int64_t) self->read_pos + offset;
  } else if (whence == SEEK_END) {
    if (!self->upstream_eos && self->total_size <= 0) {
      while (!self->stop_worker && !self->upstream_eos) {
        g_cond_wait (&self->io_cond, &self->io_lock);
      }
    }
    int64_t base = self->upstream_eos ? (int64_t) self->write_pos : (int64_t) self->total_size;
    if (base >= 0)
      target = base + offset;
  }

  if (target < 0) {
    g_mutex_unlock (&self->io_lock);
    return AVERROR (EINVAL);
  }

  while (!self->stop_worker && !self->upstream_eos && (guint64) target > self->write_pos) {
    g_cond_wait (&self->io_cond, &self->io_lock);
  }
  if (self->stop_worker) {
    g_mutex_unlock (&self->io_lock);
    return AVERROR_EXIT;
  }

  if ((guint64) target > self->write_pos && self->upstream_eos) {
    self->read_pos = self->write_pos;
  } else {
    self->read_pos = (guint64) target;
  }
  int64_t res = (int64_t) self->read_pos;
  g_mutex_unlock (&self->io_lock);
  return res;
}

static gboolean
olaz_av_src_query (GstPad *pad, GstObject *parent, GstQuery *query)
{
  OlazAvDemux *self = OLAZ_AV_DEMUX (parent);
  switch (GST_QUERY_TYPE (query)) {
    case GST_QUERY_DURATION: {
      GstFormat fmt;
      gst_query_parse_duration (query, &fmt, NULL);
      g_mutex_lock (&self->io_lock);
      GstClockTime dur = self->duration_ns;
      g_mutex_unlock (&self->io_lock);
      if (fmt == GST_FORMAT_TIME && GST_CLOCK_TIME_IS_VALID (dur)) {
        gst_query_set_duration (query, GST_FORMAT_TIME, (gint64) dur);
        return TRUE;
      }
      return FALSE;
    }
    case GST_QUERY_SEEKING: {
      GstFormat fmt;
      gst_query_parse_seeking (query, &fmt, NULL, NULL, NULL);
      if (fmt == GST_FORMAT_TIME) {
        g_mutex_lock (&self->io_lock);
        GstClockTime dur = self->duration_ns;
        g_mutex_unlock (&self->io_lock);
        gint64 end = GST_CLOCK_TIME_IS_VALID (dur) ? (gint64) dur : -1;
        gst_query_set_seeking (query, GST_FORMAT_TIME, TRUE, 0, end);
        return TRUE;
      }
      return FALSE;
    }
    default:
      return gst_pad_query_default (pad, parent, query);
  }
}

static gboolean
olaz_av_src_event (GstPad *pad, GstObject *parent, GstEvent *event)
{
  OlazAvDemux *self = OLAZ_AV_DEMUX (parent);
  if (GST_EVENT_TYPE (event) == GST_EVENT_SEEK) {
    gdouble rate = 1.0;
    GstFormat fmt = GST_FORMAT_UNDEFINED;
    GstSeekFlags flags = GST_SEEK_FLAG_NONE;
    GstSeekType start_type = GST_SEEK_TYPE_NONE;
    gint64 start = 0;
    GstSeekType stop_type = GST_SEEK_TYPE_NONE;
    gint64 stop = -1;
    gst_event_parse_seek (event, &rate, &fmt, &flags, &start_type, &start, &stop_type, &stop);
    guint32 seq = gst_event_get_seqnum (event);

    if (fmt == GST_FORMAT_TIME) {
      gboolean do_flush = FALSE;
      GstPad *apad = NULL;
      GstPad *vpad = NULL;

      gboolean is_new_seek = FALSE;
      gint64 target_ns = (start_type == GST_SEEK_TYPE_SET && start >= 0) ? start : 0;
      g_mutex_lock (&self->io_lock);
      if (seq != self->last_seek_seqnum) {
        self->last_seek_seqnum = seq;
        is_new_seek = TRUE;
        do_flush = (flags & GST_SEEK_FLAG_FLUSH) != 0;
        if (self->audio_pad != NULL)
          apad = GST_PAD (gst_object_ref (self->audio_pad));
        if (self->video_pad != NULL)
          vpad = GST_PAD (gst_object_ref (self->video_pad));
      }
      g_mutex_unlock (&self->io_lock);

      if (is_new_seek) {
        g_mutex_lock (&self->io_lock);
        self->pending_seek_ns = target_ns;
        self->pending_seek_flush = do_flush;
        self->pending_seek_rate = (rate != 0.0) ? rate : 1.0;
        g_cond_broadcast (&self->io_cond);
        g_mutex_unlock (&self->io_lock);

        if (do_flush) {
          if (apad != NULL) {
            GstEvent *fs = gst_event_new_flush_start ();
            gst_event_set_seqnum (fs, seq);
            gst_pad_push_event (apad, fs);
          }
          if (vpad != NULL) {
            GstEvent *fs = gst_event_new_flush_start ();
            gst_event_set_seqnum (fs, seq);
            gst_pad_push_event (vpad, fs);
          }
        }
        if (apad != NULL)
          gst_object_unref (apad);
        if (vpad != NULL)
          gst_object_unref (vpad);
      } gst_event_unref (event);
      return TRUE;
    }
  }
  return gst_pad_event_default (pad, parent, event);
}

static void
olaz_av_push_segment_to (GstPad *pad, GstClockTime start_ns, GstClockTime dur_ns,
    gdouble rate, guint32 seq)
{
  if (pad == NULL)
    return;
  GstSegment seg;
  gst_segment_init (&seg, GST_FORMAT_TIME);
  seg.rate = (rate != 0.0) ? rate : 1.0;
  seg.start = start_ns;
  seg.time = start_ns;
  seg.position = start_ns;
  seg.duration = dur_ns;
  GstEvent *ev = gst_event_new_segment (&seg);
  if (seq != 0)
    gst_event_set_seqnum (ev, seq);
  gst_pad_push_event (pad, ev);
}

static gboolean
olaz_av_has_pending_seek_or_stop (OlazAvDemux *self)
{
  g_mutex_lock (&self->io_lock);
  gboolean bail = self->stop_worker || self->pending_seek_ns >= 0;
  g_mutex_unlock (&self->io_lock);
  return bail;
}

static GstFlowReturn
olaz_av_drain_audio (OlazAvDemux *self, AVCodecContext *actx, AVStream *ast,
    SwrContext *swr, int out_rate, int out_ch, AVFrame *frame,
    GstClockTime *next_pts, GstClockTime seg_start_ns)
{
  while (TRUE) {
    if (olaz_av_has_pending_seek_or_stop (self))
      return GST_FLOW_FLUSHING;

    int r = avcodec_receive_frame (actx, frame);
    if (r == AVERROR (EAGAIN) || r == AVERROR_EOF || r < 0)
      break;

    int max_out = swr_get_out_samples (swr, frame->nb_samples);
    if (max_out <= 0)
      max_out = frame->nb_samples + 256;

    gsize bpf = (gsize) out_ch * 2u;
    GstBuffer *buf = gst_buffer_new_allocate (NULL, (gsize) max_out * bpf, NULL);
    GstMapInfo map;
    gst_buffer_map (buf, &map, GST_MAP_WRITE);
    uint8_t *out_planes[1] = { map.data };
    int converted = swr_convert (swr, out_planes, max_out,
        (const uint8_t **) frame->extended_data, frame->nb_samples);
    gst_buffer_unmap (buf, &map);

    if (converted <= 0) {
      gst_buffer_unref (buf);
      av_frame_unref (frame);
      continue;
    }
    gst_buffer_set_size (buf, (gsize) converted * bpf);

    GstClockTime pts = *next_pts;
    int64_t ts = (frame->best_effort_timestamp != AV_NOPTS_VALUE)
        ? frame->best_effort_timestamp
        : frame->pts;
    if (ts != AV_NOPTS_VALUE && ast->time_base.den > 0) {
      int64_t ns = av_rescale_q (ts, ast->time_base, (AVRational) { 1, GST_SECOND });
      if (ns >= 0)
        pts = (GstClockTime) ns;
    }
    GstClockTime dur = gst_util_uint64_scale_int ((guint64) converted, GST_SECOND, out_rate);
    *next_pts = pts + dur;
    av_frame_unref (frame);

    if (pts + dur <= seg_start_ns || olaz_av_has_pending_seek_or_stop (self)) {
      gst_buffer_unref (buf);
      continue;
    }

    GST_BUFFER_PTS (buf) = pts;
    GST_BUFFER_DTS (buf) = pts;
    GST_BUFFER_DURATION (buf) = dur;

    GstFlowReturn fret = gst_pad_push (self->audio_pad, buf);
    if (fret != GST_FLOW_OK)
      return fret;
  }
  return GST_FLOW_OK;
}

static GstFlowReturn
olaz_av_drain_video (OlazAvDemux *self, AVCodecContext *vctx, AVStream *vst,
    struct SwsContext **sws_ptr, GstVideoInfo *vinfo, AVFrame *frame,
    GstClockTime *next_pts, GstClockTime seg_start_ns)
{
  while (TRUE) {
    if (olaz_av_has_pending_seek_or_stop (self))
      return GST_FLOW_FLUSHING;

    int r = avcodec_receive_frame (vctx, frame);
    if (r == AVERROR (EAGAIN) || r == AVERROR_EOF || r < 0)
      break;

    if (frame->width <= 0 || frame->height <= 0) {
      av_frame_unref (frame);
      continue;
    }

    GstClockTime pts = *next_pts;
    int64_t ts = (frame->best_effort_timestamp != AV_NOPTS_VALUE)
        ? frame->best_effort_timestamp
        : frame->pts;
    if (ts != AV_NOPTS_VALUE && vst->time_base.den > 0) {
      int64_t ns = av_rescale_q (ts, vst->time_base, (AVRational) { 1, GST_SECOND });
      if (ns >= 0)
        pts = (GstClockTime) ns;
    }
    GstClockTime dur = (GST_VIDEO_INFO_FPS_N (vinfo) > 0 && GST_VIDEO_INFO_FPS_D (vinfo) > 0)
        ? gst_util_uint64_scale_int (GST_SECOND, GST_VIDEO_INFO_FPS_D (vinfo), GST_VIDEO_INFO_FPS_N (vinfo))
        : (GST_SECOND / 30);
    if (frame->duration > 0 && vst->time_base.den > 0) {
      int64_t dns = av_rescale_q (frame->duration, vst->time_base, (AVRational) { 1, GST_SECOND });
      if (dns > 0)
        dur = (GstClockTime) dns;
    }
    *next_pts = pts + dur;

    if (pts + dur <= seg_start_ns || olaz_av_has_pending_seek_or_stop (self)) {
      av_frame_unref (frame);
      continue;
    }

    *sws_ptr = sws_getCachedContext (*sws_ptr,
        frame->width, frame->height, (enum AVPixelFormat) frame->format,
        GST_VIDEO_INFO_WIDTH (vinfo), GST_VIDEO_INFO_HEIGHT (vinfo), AV_PIX_FMT_YUV420P,
        SWS_BILINEAR, NULL, NULL, NULL);
    if (*sws_ptr == NULL) {
      av_frame_unref (frame);
      continue;
    }

    GstBuffer *buf = gst_buffer_new_allocate (NULL, GST_VIDEO_INFO_SIZE (vinfo), NULL);
    GstVideoFrame vframe;
    if (!gst_video_frame_map (&vframe, vinfo, buf, GST_MAP_WRITE)) {
      gst_buffer_unref (buf);
      av_frame_unref (frame);
      continue;
    }

    uint8_t *dst_data[4] = {
      (uint8_t *) GST_VIDEO_FRAME_PLANE_DATA (&vframe, 0),
      (uint8_t *) GST_VIDEO_FRAME_PLANE_DATA (&vframe, 1),
      (uint8_t *) GST_VIDEO_FRAME_PLANE_DATA (&vframe, 2),
      NULL,
    };
    int dst_stride[4] = {
      GST_VIDEO_FRAME_PLANE_STRIDE (&vframe, 0),
      GST_VIDEO_FRAME_PLANE_STRIDE (&vframe, 1),
      GST_VIDEO_FRAME_PLANE_STRIDE (&vframe, 2),
      0,
    };
    sws_scale (*sws_ptr, (const uint8_t *const *) frame->data, frame->linesize,
        0, frame->height, dst_data, dst_stride);
    gst_video_frame_unmap (&vframe);
    av_frame_unref (frame);

    GST_BUFFER_PTS (buf) = pts;
    GST_BUFFER_DTS (buf) = pts;
    GST_BUFFER_DURATION (buf) = dur;

    GstFlowReturn fret = gst_pad_push (self->video_pad, buf);
    if (fret != GST_FLOW_OK)
      return fret;
  }
  return GST_FLOW_OK;
}

static gpointer
olaz_av_worker_main (gpointer data)
{
  OlazAvDemux *self = OLAZ_AV_DEMUX (data);
  AVFormatContext *fmt_ctx = NULL;
  AVIOContext *avio_ctx = NULL;
  AVCodecContext *actx = NULL;
  AVCodecContext *vctx = NULL;
  SwrContext *swr = NULL;
  struct SwsContext *sws = NULL;
  AVPacket *pkt = NULL;
  AVFrame *frame = NULL;
  int audio_idx = -1;
  int video_idx = -1;
  int out_rate = 48000;
  int out_ch = 2;
  GstVideoInfo vinfo;

  unsigned char *avio_buf = (unsigned char *) av_malloc (32768);
  if (avio_buf == NULL)
    goto beach;

  avio_ctx = avio_alloc_context (avio_buf, 32768, 0, self, olaz_avio_read, NULL, olaz_avio_seek);
  if (avio_ctx == NULL) {
    av_free (avio_buf);
    goto beach;
  }
  avio_ctx->seekable = AVIO_SEEKABLE_NORMAL;

  fmt_ctx = avformat_alloc_context ();
  fmt_ctx->pb = avio_ctx;
  fmt_ctx->flags |= AVFMT_FLAG_CUSTOM_IO;

  if (avformat_open_input (&fmt_ctx, NULL, NULL, NULL) < 0) {
    GST_ELEMENT_ERROR (self, STREAM, DEMUX, ("FFmpeg failed to open media stream"), (NULL));
    goto beach;
  }
  if (avformat_find_stream_info (fmt_ctx, NULL) < 0) {
    GST_ELEMENT_ERROR (self, STREAM, DEMUX, ("FFmpeg failed to parse stream info"), (NULL));
    goto beach;
  }

  GstClockTime dur_ns = GST_CLOCK_TIME_NONE;
  if (fmt_ctx->duration > 0 && fmt_ctx->duration != AV_NOPTS_VALUE) {
    dur_ns = (GstClockTime) av_rescale_q (fmt_ctx->duration,
        (AVRational) { 1, AV_TIME_BASE }, (AVRational) { 1, GST_SECOND });
  }

  for (unsigned int i = 0; i < fmt_ctx->nb_streams; i++) {
    AVStream *st = fmt_ctx->streams[i];
    if (st->duration > 0 && st->duration != AV_NOPTS_VALUE && st->time_base.den > 0) {
      GstClockTime sd = (GstClockTime) av_rescale_q (st->duration,
          st->time_base, (AVRational) { 1, GST_SECOND });
      if (!GST_CLOCK_TIME_IS_VALID (dur_ns) || sd > dur_ns)
        dur_ns = sd;
    }
    if (audio_idx < 0 && st->codecpar->codec_type == AVMEDIA_TYPE_AUDIO) {
      audio_idx = (int) i;
    } else if (video_idx < 0 && st->codecpar->codec_type == AVMEDIA_TYPE_VIDEO &&
        (st->disposition & AV_DISPOSITION_ATTACHED_PIC) == 0) {
      video_idx = (int) i;
    }
  }

  g_mutex_lock (&self->io_lock);
  self->duration_ns = dur_ns;
  g_mutex_unlock (&self->io_lock);

  if (audio_idx >= 0) {
    AVStream *ast = fmt_ctx->streams[audio_idx];
    const AVCodec *acodec = avcodec_find_decoder (ast->codecpar->codec_id);
    if (acodec != NULL) {
      actx = avcodec_alloc_context3 (acodec);
      if (avcodec_parameters_to_context (actx, ast->codecpar) >= 0 &&
          avcodec_open2 (actx, acodec, NULL) >= 0) {
        out_rate = (actx->sample_rate > 0) ? actx->sample_rate : 48000;
        out_ch = (actx->ch_layout.nb_channels > 0)
            ? CLAMP (actx->ch_layout.nb_channels, 1, 2)
            : 2;
        AVChannelLayout out_layout = { 0 };
        av_channel_layout_default (&out_layout, out_ch);
        if (actx->ch_layout.nb_channels <= 0)
          av_channel_layout_default (&actx->ch_layout, out_ch);

        if (swr_alloc_set_opts2 (&swr, &out_layout, AV_SAMPLE_FMT_S16, out_rate,
                &actx->ch_layout, actx->sample_fmt, out_rate, 0, NULL) >= 0 &&
            swr_init (swr) >= 0) {
          GstPad *apad = gst_pad_new_from_static_template (&olaz_av_audio_src_template, "audio_0");
          gst_pad_use_fixed_caps (apad);
          gst_pad_set_query_function (apad, GST_DEBUG_FUNCPTR (olaz_av_src_query));
          gst_pad_set_event_function (apad, GST_DEBUG_FUNCPTR (olaz_av_src_event));
          gst_pad_set_active (apad, TRUE);

          char *sid = gst_pad_create_stream_id (apad, GST_ELEMENT_CAST (self), "audio");
        GstEvent *ss_ev = gst_event_new_stream_start (sid);
        GstEvent *up_ss = gst_pad_get_sticky_event (self->sinkpad, GST_EVENT_STREAM_START, 0);
        if (up_ss != NULL) {
          guint gid = 0;
          if (gst_event_parse_group_id (up_ss, &gid))
            gst_event_set_group_id (ss_ev, gid);
          gst_event_unref (up_ss);
        } else {
          gst_event_set_group_id (ss_ev, 1);
        }
        gst_pad_push_event (apad, ss_ev);
        g_free (sid);

          GstCaps *acaps = gst_caps_new_simple ("audio/x-raw",
              "format", G_TYPE_STRING, "S16LE",
              "layout", G_TYPE_STRING, "interleaved",
              "rate", G_TYPE_INT, out_rate,
              "channels", G_TYPE_INT, out_ch,
              NULL);
          if (out_ch >= 2) {
            guint64 mask = gst_audio_channel_get_fallback_mask (out_ch);
            if (mask != 0)
              gst_caps_set_simple (acaps, "channel-mask", GST_TYPE_BITMASK, mask, NULL);
          }
          gst_pad_set_caps (apad, acaps);
          gst_caps_unref (acaps);

          g_mutex_lock (&self->io_lock);
          self->audio_pad = apad;
          g_mutex_unlock (&self->io_lock);
          gst_element_add_pad (GST_ELEMENT (self), apad);
        }
        av_channel_layout_uninit (&out_layout);
      }
    }
    if (self->audio_pad == NULL)
      audio_idx = -1;
  }

  if (video_idx >= 0) {
    AVStream *vst = fmt_ctx->streams[video_idx];
    const AVCodec *vcodec = avcodec_find_decoder (vst->codecpar->codec_id);
    if (vcodec != NULL) {
      vctx = avcodec_alloc_context3 (vcodec);
      vctx->thread_count = 2;
      if (avcodec_parameters_to_context (vctx, vst->codecpar) >= 0 &&
          avcodec_open2 (vctx, vcodec, NULL) >= 0 &&
          vctx->width > 0 && vctx->height > 0) {
        AVRational fr = vst->avg_frame_rate;
        if (fr.num <= 0 || fr.den <= 0)
          fr = vst->r_frame_rate;
        if (fr.num <= 0 || fr.den <= 0)
          fr = (AVRational) { 30, 1 };

        gst_video_info_init (&vinfo);
        gst_video_info_set_format (&vinfo, GST_VIDEO_FORMAT_I420,
            (guint) vctx->width, (guint) vctx->height);
        GST_VIDEO_INFO_FPS_N (&vinfo) = fr.num;
        GST_VIDEO_INFO_FPS_D (&vinfo) = fr.den;

        GstPad *vpad = gst_pad_new_from_static_template (&olaz_av_video_src_template, "video_0");
        gst_pad_use_fixed_caps (vpad);
        gst_pad_set_query_function (vpad, GST_DEBUG_FUNCPTR (olaz_av_src_query));
        gst_pad_set_event_function (vpad, GST_DEBUG_FUNCPTR (olaz_av_src_event));
        gst_pad_set_active (vpad, TRUE);

        char *sid = gst_pad_create_stream_id (vpad, GST_ELEMENT_CAST (self), "video");
        GstEvent *ss_ev = gst_event_new_stream_start (sid);
        GstEvent *up_ss = gst_pad_get_sticky_event (self->sinkpad, GST_EVENT_STREAM_START, 0);
        if (up_ss != NULL) {
          guint gid = 0;
          if (gst_event_parse_group_id (up_ss, &gid))
            gst_event_set_group_id (ss_ev, gid);
          gst_event_unref (up_ss);
        } else {
          gst_event_set_group_id (ss_ev, 1);
        }
        gst_pad_push_event (vpad, ss_ev);
        g_free (sid);

        GstCaps *vcaps = gst_video_info_to_caps (&vinfo);
        gst_pad_set_caps (vpad, vcaps);
        gst_caps_unref (vcaps);

        g_mutex_lock (&self->io_lock);
        self->video_pad = vpad;
        g_mutex_unlock (&self->io_lock);
        gst_element_add_pad (GST_ELEMENT (self), vpad);
      }
    }
    if (self->video_pad == NULL)
      video_idx = -1;
  }

  gst_element_no_more_pads (GST_ELEMENT (self));

  if (self->audio_pad == NULL && self->video_pad == NULL) {
    GST_ELEMENT_ERROR (self, STREAM, DECODE, ("No decodable audio or video stream found"), (NULL));
    goto beach;
  }

  olaz_av_push_segment_to (self->audio_pad, 0, dur_ns, 1.0, 0);
  olaz_av_push_segment_to (self->video_pad, 0, dur_ns, 1.0, 0);

  pkt = av_packet_alloc ();
  frame = av_frame_alloc ();
  GstClockTime next_audio_pts = 0;
  GstClockTime next_video_pts = 0;
  GstClockTime seg_start_ns = 0;

  while (TRUE) {
    g_mutex_lock (&self->io_lock);
    gboolean stop = self->stop_worker;
    gint64 seek_ns = self->pending_seek_ns;
    gboolean seek_flush = self->pending_seek_flush;
    gdouble seek_rate = self->pending_seek_rate;
    guint32 seek_seq = self->last_seek_seqnum;
    if (seek_ns >= 0) {
      self->pending_seek_ns = -1;
      self->pending_seek_flush = FALSE;
    }
    g_mutex_unlock (&self->io_lock);

    if (stop)
      break;

    if (seek_ns >= 0) {
      if (seek_flush) {
        if (self->audio_pad != NULL) {
          GstEvent *fstop = gst_event_new_flush_stop (TRUE);
          gst_event_set_seqnum (fstop, seek_seq);
          gst_pad_push_event (self->audio_pad, fstop);
        }
        if (self->video_pad != NULL) {
          GstEvent *fstop = gst_event_new_flush_stop (TRUE);
          gst_event_set_seqnum (fstop, seek_seq);
          gst_pad_push_event (self->video_pad, fstop);
        }
      }
      int64_t seek_tb = av_rescale_q (seek_ns,
          (AVRational) { 1, GST_SECOND }, (AVRational) { 1, AV_TIME_BASE });
      av_seek_frame (fmt_ctx, -1, seek_tb, AVSEEK_FLAG_BACKWARD);
      if (actx != NULL)
        avcodec_flush_buffers (actx);
      if (vctx != NULL)
        avcodec_flush_buffers (vctx);
      if (swr != NULL)
        swr_init (swr);

      seg_start_ns = (GstClockTime) seek_ns;
      next_audio_pts = seg_start_ns;
      next_video_pts = seg_start_ns;
      olaz_av_push_segment_to (self->audio_pad, seg_start_ns, dur_ns, seek_rate, seek_seq);
      olaz_av_push_segment_to (self->video_pad, seg_start_ns, dur_ns, seek_rate, seek_seq);
    }

    int r = av_read_frame (fmt_ctx, pkt);
    if (r < 0) {
      if (r == AVERROR_EXIT)
        break;

      if (actx != NULL && self->audio_pad != NULL) {
        avcodec_send_packet (actx, NULL);
        olaz_av_drain_audio (self, actx, fmt_ctx->streams[audio_idx],
            swr, out_rate, out_ch, frame, &next_audio_pts, seg_start_ns);
      }
      if (vctx != NULL && self->video_pad != NULL) {
        avcodec_send_packet (vctx, NULL);
        olaz_av_drain_video (self, vctx, fmt_ctx->streams[video_idx],
            &sws, &vinfo, frame, &next_video_pts, seg_start_ns);
      }
      if (self->audio_pad != NULL)
        gst_pad_push_event (self->audio_pad, gst_event_new_eos ());
      if (self->video_pad != NULL)
        gst_pad_push_event (self->video_pad, gst_event_new_eos ());

      /* Wait for either pipeline teardown or a post-EOS seek/replay (`currentTime = 0`) */
      g_mutex_lock (&self->io_lock);
      while (!self->stop_worker && self->pending_seek_ns < 0) {
        g_cond_wait (&self->io_cond, &self->io_lock);
      }
      gboolean stop_after_eos = self->stop_worker;
      g_mutex_unlock (&self->io_lock);
      if (stop_after_eos)
        break;
      continue;
    }

    if (pkt->stream_index == audio_idx && actx != NULL && self->audio_pad != NULL) {
      if (avcodec_send_packet (actx, pkt) >= 0) {
        olaz_av_drain_audio (self, actx, fmt_ctx->streams[audio_idx],
            swr, out_rate, out_ch, frame, &next_audio_pts, seg_start_ns);
      }
    } else if (pkt->stream_index == video_idx && vctx != NULL && self->video_pad != NULL) {
      if (avcodec_send_packet (vctx, pkt) >= 0) {
        olaz_av_drain_video (self, vctx, fmt_ctx->streams[video_idx],
            &sws, &vinfo, frame, &next_video_pts, seg_start_ns);
      }
    }
    av_packet_unref (pkt);
  }

beach:
  av_frame_free (&frame);
  av_packet_free (&pkt);
  sws_freeContext (sws);
  swr_free (&swr);
  avcodec_free_context (&vctx);
  avcodec_free_context (&actx);
  if (fmt_ctx != NULL)
    avformat_close_input (&fmt_ctx);
  if (avio_ctx != NULL) {
    av_freep (&avio_ctx->buffer);
    avio_context_free (&avio_ctx);
  }
  gst_object_unref (self);
  return NULL;
}

static void
olaz_av_maybe_start_worker_locked (OlazAvDemux *self)
{
  if (!self->worker_started && !self->stop_worker &&
      (self->write_pos >= 16384 || (self->upstream_eos && self->write_pos > 0))) {
    self->worker_started = TRUE;
    self->worker_thread = g_thread_new ("olaz-avdec", olaz_av_worker_main, gst_object_ref (self));
  }
}

static GstFlowReturn
olaz_av_sink_chain (GstPad *pad, GstObject *parent, GstBuffer *buf)
{
  (void) pad;
  OlazAvDemux *self = OLAZ_AV_DEMUX (parent);

  if (!self->queried_upstream_size) {
    self->queried_upstream_size = TRUE;
    gint64 bdur = -1;
    if (gst_pad_peer_query_duration (self->sinkpad, GST_FORMAT_BYTES, &bdur) && bdur > 0) {
      g_mutex_lock (&self->io_lock);
      if (self->total_size < 0)
        self->total_size = bdur;
      g_mutex_unlock (&self->io_lock);
    }
  }

  GstMapInfo map;
  if (!gst_buffer_map (buf, &map, GST_MAP_READ)) {
    gst_buffer_unref (buf);
    return GST_FLOW_ERROR;
  }

  g_mutex_lock (&self->io_lock);
  if (self->stop_worker || self->memfd < 0) {
    g_mutex_unlock (&self->io_lock);
    gst_buffer_unmap (buf, &map);
    gst_buffer_unref (buf);
    return GST_FLOW_FLUSHING;
  }
  int fd = self->memfd;
  guint64 off = self->write_pos;
  g_mutex_unlock (&self->io_lock);

  gsize remaining = map.size;
  const guint8 *ptr = map.data;
  while (remaining > 0) {
    ssize_t w = pwrite (fd, ptr, remaining, (off_t) off);
    if (w <= 0)
      break;
    ptr += w;
    off += (guint64) w;
    remaining -= (gsize) w;
  }
  gst_buffer_unmap (buf, &map);
  gst_buffer_unref (buf);

  g_mutex_lock (&self->io_lock);
  self->write_pos = off;
  olaz_av_maybe_start_worker_locked (self);
  g_cond_broadcast (&self->io_cond);
  g_mutex_unlock (&self->io_lock);

  return GST_FLOW_OK;
}

static gboolean
olaz_av_sink_event (GstPad *pad, GstObject *parent, GstEvent *event)
{
  OlazAvDemux *self = OLAZ_AV_DEMUX (parent);
  switch (GST_EVENT_TYPE (event)) {
    case GST_EVENT_STREAM_START:
      return gst_pad_event_default (pad, parent, event);

    case GST_EVENT_CAPS:
      gst_event_unref (event);
      return TRUE;

    case GST_EVENT_SEGMENT: {
      const GstSegment *seg = NULL;
      gst_event_parse_segment (event, &seg);
      if (seg != NULL && seg->format == GST_FORMAT_BYTES && seg->stop != (guint64) -1 && seg->stop > 0) {
        g_mutex_lock (&self->io_lock);
        if (self->total_size < 0)
          self->total_size = (gint64) seg->stop;
        g_mutex_unlock (&self->io_lock);
      }
      gst_event_unref (event);
      return TRUE;
    }

    case GST_EVENT_EOS:
      g_mutex_lock (&self->io_lock);
      self->upstream_eos = TRUE;
      if (self->total_size < 0)
        self->total_size = (gint64) self->write_pos;
      olaz_av_maybe_start_worker_locked (self);
      g_cond_broadcast (&self->io_cond);
      g_mutex_unlock (&self->io_lock);
      gst_event_unref (event);
      return TRUE;

    default:
      return gst_pad_event_default (pad, parent, event);
  }
}

static void
olaz_av_demux_stop_and_reset (OlazAvDemux *self)
{
  g_mutex_lock (&self->io_lock);
  self->stop_worker = TRUE;
  g_cond_broadcast (&self->io_cond);
  GThread *th = self->worker_thread;
  self->worker_thread = NULL;
  g_mutex_unlock (&self->io_lock);

  if (self->audio_pad != NULL)
    gst_pad_push_event (self->audio_pad, gst_event_new_flush_start ());
  if (self->video_pad != NULL)
    gst_pad_push_event (self->video_pad, gst_event_new_flush_start ());

  if (th != NULL)
    g_thread_join (th);

  if (self->audio_pad != NULL) {
    gst_pad_set_active (self->audio_pad, FALSE);
    gst_element_remove_pad (GST_ELEMENT (self), self->audio_pad);
    self->audio_pad = NULL;
  }
  if (self->video_pad != NULL) {
    gst_pad_set_active (self->video_pad, FALSE);
    gst_element_remove_pad (GST_ELEMENT (self), self->video_pad);
    self->video_pad = NULL;
  }

  g_mutex_lock (&self->io_lock);
  if (self->memfd >= 0) {
    close (self->memfd);
    self->memfd = -1;
  }
  self->write_pos = 0;
  self->read_pos = 0;
  self->total_size = -1;
  self->queried_upstream_size = FALSE;
  self->upstream_eos = FALSE;
  self->worker_started = FALSE;
  self->pending_seek_ns = -1;
  self->pending_seek_flush = FALSE;
  self->pending_seek_rate = 1.0;
  self->last_seek_seqnum = 0;
  self->duration_ns = GST_CLOCK_TIME_NONE;
  g_mutex_unlock (&self->io_lock);
}

static GstStateChangeReturn
olaz_av_demux_change_state (GstElement *element, GstStateChange transition)
{
  OlazAvDemux *self = OLAZ_AV_DEMUX (element);

  if (transition == GST_STATE_CHANGE_READY_TO_PAUSED) {
    olaz_av_demux_stop_and_reset (self);
    g_mutex_lock (&self->io_lock);
    self->stop_worker = FALSE;
    self->memfd = memfd_create ("olaz-av", MFD_CLOEXEC);
    if (self->memfd < 0) {
      FILE *tf = tmpfile ();
      if (tf != NULL) {
        self->memfd = dup (fileno (tf));
        fclose (tf);
      }
    }
    g_mutex_unlock (&self->io_lock);
    if (self->memfd < 0)
      return GST_STATE_CHANGE_FAILURE;
  }

  if (transition == GST_STATE_CHANGE_PAUSED_TO_READY) {
    olaz_av_demux_stop_and_reset (self);
  }

  return GST_ELEMENT_CLASS (olaz_av_demux_parent_class)->change_state (element, transition);
}

static void
olaz_av_demux_finalize (GObject *object)
{
  OlazAvDemux *self = OLAZ_AV_DEMUX (object);
  olaz_av_demux_stop_and_reset (self);
  g_cond_clear (&self->io_cond);
  g_mutex_clear (&self->io_lock);
  G_OBJECT_CLASS (olaz_av_demux_parent_class)->finalize (object);
}

static void
olaz_av_demux_init (OlazAvDemux *self)
{
  g_mutex_init (&self->io_lock);
  g_cond_init (&self->io_cond);
  self->memfd = -1;
  self->total_size = -1;
  self->pending_seek_ns = -1;
  self->pending_seek_flush = FALSE;
  self->pending_seek_rate = 1.0;
  self->duration_ns = GST_CLOCK_TIME_NONE;

  self->sinkpad = gst_pad_new_from_static_template (&olaz_av_sink_full_template, "sink");
  gst_pad_set_chain_function (self->sinkpad, GST_DEBUG_FUNCPTR (olaz_av_sink_chain));
  gst_pad_set_event_function (self->sinkpad, GST_DEBUG_FUNCPTR (olaz_av_sink_event));
  gst_element_add_pad (GST_ELEMENT (self), self->sinkpad);
}

static void
olaz_av_demux_class_init (OlazAvDemuxClass *klass)
{
  GObjectClass *gobject_class = G_OBJECT_CLASS (klass);
  GstElementClass *element_class = GST_ELEMENT_CLASS (klass);

  gobject_class->finalize = olaz_av_demux_finalize;
  element_class->change_state = olaz_av_demux_change_state;

  gst_element_class_set_static_metadata (element_class,
      "Olaz FFmpeg Universal Demuxer/Decoder",
      "Codec/Demuxer",
      "Demuxes and decodes MP4/M4A/AAC/H.264/MP3/AMR/WebM/Ogg via FFmpeg",
      "Olaz Contributors");
  gst_element_class_add_static_pad_template (element_class, &olaz_av_sink_full_template);
  gst_element_class_add_static_pad_template (element_class, &olaz_av_audio_src_template);
  gst_element_class_add_static_pad_template (element_class, &olaz_av_video_src_template);
}

static gboolean
olaz_system_has_full_mp4_decoders (void)
{
  static const char *const kH264[] = { "avdec_h264", "vah264dec", "v4l2slh264dec", "openh264dec" };
  static const char *const kAac[] = { "avdec_aac", "fdkaacdec", "faad" };

  GstElementFactory *qtd = gst_element_factory_find ("qtdemux");
  if (qtd == NULL)
    return FALSE;
  gst_object_unref (qtd);

  gboolean has_h264 = FALSE;
  for (gsize i = 0; i < G_N_ELEMENTS (kH264); i++) {
    GstElementFactory *f = gst_element_factory_find (kH264[i]);
    if (f != NULL) {
      gst_object_unref (f);
      has_h264 = TRUE;
      break;
    }
  }
  gboolean has_aac = FALSE;
  for (gsize i = 0; i < G_N_ELEMENTS (kAac); i++) {
    GstElementFactory *f = gst_element_factory_find (kAac[i]);
    if (f != NULL) {
      gst_object_unref (f);
      has_aac = TRUE;
      break;
    }
  }
  return has_h264 && has_aac;
}


typedef struct _OlazAvVideoCaps { GstElement parent; } OlazAvVideoCaps;
typedef struct _OlazAvVideoCapsClass { GstElementClass parent_class; } OlazAvVideoCapsClass;
GType olaz_av_video_caps_get_type (void);
G_DEFINE_TYPE (OlazAvVideoCaps, olaz_av_video_caps, GST_TYPE_ELEMENT)

static GstStaticPadTemplate olaz_av_vcaps_sink = GST_STATIC_PAD_TEMPLATE ("sink",
    GST_PAD_SINK, GST_PAD_ALWAYS,
    GST_STATIC_CAPS ("video/x-h264; video/x-h265; video/x-vp8; video/x-vp9"));
static GstStaticPadTemplate olaz_av_vcaps_src = GST_STATIC_PAD_TEMPLATE ("src",
    GST_PAD_SRC, GST_PAD_ALWAYS, GST_STATIC_CAPS ("video/x-raw"));

static void olaz_av_video_caps_init (OlazAvVideoCaps *self) { (void) self; }
static void olaz_av_video_caps_class_init (OlazAvVideoCapsClass *klass) {
  GstElementClass *ec = GST_ELEMENT_CLASS (klass);
  gst_element_class_set_static_metadata (ec, "Olaz FFmpeg Video Codec Capability",
      "Codec/Decoder/Video", "Advertises FFmpeg video codec support to WebKit", "Olaz Contributors");
  gst_element_class_add_static_pad_template (ec, &olaz_av_vcaps_sink);
  gst_element_class_add_static_pad_template (ec, &olaz_av_vcaps_src);
}

typedef struct _OlazAvAudioCaps { GstElement parent; } OlazAvAudioCaps;
typedef struct _OlazAvAudioCapsClass { GstElementClass parent_class; } OlazAvAudioCapsClass;
GType olaz_av_audio_caps_get_type (void);
G_DEFINE_TYPE (OlazAvAudioCaps, olaz_av_audio_caps, GST_TYPE_ELEMENT)

static GstStaticPadTemplate olaz_av_acaps_sink = GST_STATIC_PAD_TEMPLATE ("sink",
    GST_PAD_SINK, GST_PAD_ALWAYS,
    GST_STATIC_CAPS ("audio/mpeg; audio/aac; audio/x-opus; audio/x-vorbis; audio/AMR; audio/AMR-WB; audio/x-flac"));
static GstStaticPadTemplate olaz_av_acaps_src = GST_STATIC_PAD_TEMPLATE ("src",
    GST_PAD_SRC, GST_PAD_ALWAYS, GST_STATIC_CAPS ("audio/x-raw"));

static void olaz_av_audio_caps_init (OlazAvAudioCaps *self) { (void) self; }
static void olaz_av_audio_caps_class_init (OlazAvAudioCapsClass *klass) {
  GstElementClass *ec = GST_ELEMENT_CLASS (klass);
  gst_element_class_set_static_metadata (ec, "Olaz FFmpeg Audio Codec Capability",
      "Codec/Decoder/Audio", "Advertises FFmpeg audio codec support to WebKit", "Olaz Contributors");
  gst_element_class_add_static_pad_template (ec, &olaz_av_acaps_sink);
  gst_element_class_add_static_pad_template (ec, &olaz_av_acaps_src);
}

#endif /* OLAZ_HAVE_FFMPEG */

/* ============================================================================
 * 6. `OlazScaletempo` (`scaletempo`) & `playsink` video filter fallbacks
 *
 * WebKit2GTK's `MediaPlayerPrivateGStreamer::createAudioSink()` instantiates
 * `scaletempo` (`GStreamer element scaletempo not found. Please install it`
 * when `gst-plugins-good` is absent), and `GstPlaySink` instantiates
 * `videobalance` + `deinterlace` (`soft-colorbalance` / `deinterlace` flags).
 * ============================================================================ */

static gboolean
olaz_system_has_gst_plugin_so (const char *so_name, const char *force_env_key)
{
  static const char *const kDirs[] = {
    "/usr/lib/gstreamer-1.0",
    "/usr/lib64/gstreamer-1.0",
    "/usr/lib/x86_64-linux-gnu/gstreamer-1.0",
    "/usr/lib/aarch64-linux-gnu/gstreamer-1.0",
    "/usr/local/lib/gstreamer-1.0",
    "/usr/local/lib64/gstreamer-1.0",
  };
  if (force_env_key != NULL) {
    const char *force_val = g_getenv (force_env_key);
    if (force_val != NULL && force_val[0] != '\0' && strcmp (force_val, "0") != 0)
      return FALSE;
  }
  for (gsize i = 0; i < G_N_ELEMENTS (kDirs); i++) {
    char *full = g_build_filename (kDirs[i], so_name, NULL);
    gboolean ok = g_file_test (full, G_FILE_TEST_IS_REGULAR);
    g_free (full);
    if (ok)
      return TRUE;
  }
  return FALSE;
}

#define OLAZ_TYPE_SCALETEMPO (olaz_scaletempo_get_type ())
G_DECLARE_FINAL_TYPE (OlazScaletempo, olaz_scaletempo, OLAZ, SCALETEMPO, GstElement)

struct _OlazScaletempo
{
  GstElement parent;
  GstPad *sinkpad;
  GstPad *srcpad;
  gdouble rate;
  guint stride_ms;
  gdouble overlap;
  guint search_ms;
  guint bpf;
  guint sample_rate;
};

enum
{
  ST_PROP_0,
  ST_PROP_RATE,
  ST_PROP_STRIDE,
  ST_PROP_OVERLAP,
  ST_PROP_SEARCH,
};

static GstStaticPadTemplate olaz_scaletempo_sink_template =
GST_STATIC_PAD_TEMPLATE ("sink",
    GST_PAD_SINK,
    GST_PAD_ALWAYS,
    GST_STATIC_CAPS (
        "audio/x-raw, "
        "format = (string) { S16LE, S32LE, F32LE, F64LE }, "
        "rate = (int) [ 1, 384000 ], "
        "channels = (int) [ 1, 32 ], "
        "layout = (string) interleaved"));

static GstStaticPadTemplate olaz_scaletempo_src_template =
GST_STATIC_PAD_TEMPLATE ("src",
    GST_PAD_SRC,
    GST_PAD_ALWAYS,
    GST_STATIC_CAPS (
        "audio/x-raw, "
        "format = (string) { S16LE, S32LE, F32LE, F64LE }, "
        "rate = (int) [ 1, 384000 ], "
        "channels = (int) [ 1, 32 ], "
        "layout = (string) interleaved"));

G_DEFINE_TYPE (OlazScaletempo, olaz_scaletempo, GST_TYPE_ELEMENT)

static void
olaz_scaletempo_set_property (GObject *object, guint prop_id,
    const GValue *value, GParamSpec *pspec)
{
  OlazScaletempo *self = OLAZ_SCALETEMPO (object);
  switch (prop_id) {
    case ST_PROP_STRIDE:
      self->stride_ms = g_value_get_uint (value);
      break;
    case ST_PROP_OVERLAP:
      self->overlap = g_value_get_double (value);
      break;
    case ST_PROP_SEARCH:
      self->search_ms = g_value_get_uint (value);
      break;
    default:
      G_OBJECT_WARN_INVALID_PROPERTY_ID (object, prop_id, pspec);
      break;
  }
}

static void
olaz_scaletempo_get_property (GObject *object, guint prop_id,
    GValue *value, GParamSpec *pspec)
{
  OlazScaletempo *self = OLAZ_SCALETEMPO (object);
  switch (prop_id) {
    case ST_PROP_RATE:
      g_value_set_double (value, self->rate);
      break;
    case ST_PROP_STRIDE:
      g_value_set_uint (value, self->stride_ms);
      break;
    case ST_PROP_OVERLAP:
      g_value_set_double (value, self->overlap);
      break;
    case ST_PROP_SEARCH:
      g_value_set_uint (value, self->search_ms);
      break;
    default:
      G_OBJECT_WARN_INVALID_PROPERTY_ID (object, prop_id, pspec);
      break;
  }
}

static gboolean
olaz_scaletempo_sink_event (GstPad *pad, GstObject *parent, GstEvent *event)
{
  OlazScaletempo *self = OLAZ_SCALETEMPO (parent);
  switch (GST_EVENT_TYPE (event)) {
    case GST_EVENT_CAPS: {
      GstCaps *caps = NULL;
      gst_event_parse_caps (event, &caps);
      if (caps != NULL) {
        GstAudioInfo info;
        if (gst_audio_info_from_caps (&info, caps)) {
          self->bpf = (guint) GST_AUDIO_INFO_BPF (&info);
          self->sample_rate = (guint) GST_AUDIO_INFO_RATE (&info);
        }
      }
      return gst_pad_push_event (self->srcpad, event);
    }
    case GST_EVENT_SEGMENT: {
      const GstSegment *in_seg = NULL;
      gst_event_parse_segment (event, &in_seg);
      if (in_seg != NULL && in_seg->format == GST_FORMAT_TIME) {
        gdouble new_rate = (in_seg->rate > 0.0) ? in_seg->rate : 1.0;
        self->rate = new_rate;
        if (fabs (new_rate - 1.0) > 1e-3) {
          GstSegment out_seg = *in_seg;
          gdouble prev_applied = (in_seg->applied_rate != 0.0) ? in_seg->applied_rate : 1.0;
          out_seg.applied_rate = prev_applied * new_rate;
          out_seg.rate = 1.0;
          GstEvent *new_ev = gst_event_new_segment (&out_seg);
          gst_event_set_seqnum (new_ev, gst_event_get_seqnum (event));
          gst_event_unref (event);
          return gst_pad_push_event (self->srcpad, new_ev);
        }
      }
      return gst_pad_push_event (self->srcpad, event);
    }
    default:
      return gst_pad_event_default (pad, parent, event);
  }
}

static gboolean
olaz_scaletempo_sink_query (GstPad *pad, GstObject *parent, GstQuery *query)
{
  OlazScaletempo *self = OLAZ_SCALETEMPO (parent);
  if (GST_QUERY_TYPE (query) == GST_QUERY_CAPS) {
    GstCaps *filter = NULL;
    gst_query_parse_caps (query, &filter);
    GstCaps *peer = gst_pad_peer_query_caps (self->srcpad, filter);
    if (peer != NULL) {
      GstCaps *tmpl = gst_pad_get_pad_template_caps (pad);
      GstCaps *res = gst_caps_intersect_full (peer, tmpl, GST_CAPS_INTERSECT_FIRST);
      gst_caps_unref (tmpl);
      gst_caps_unref (peer);
      gst_query_set_caps_result (query, res);
      gst_caps_unref (res);
      return TRUE;
    }
  }
  return gst_pad_query_default (pad, parent, query);
}

static GstFlowReturn
olaz_scaletempo_chain (GstPad *pad, GstObject *parent, GstBuffer *buf)
{
  (void) pad;
  OlazScaletempo *self = OLAZ_SCALETEMPO (parent);
  gdouble rate = self->rate;
  if (rate <= 0.0 || fabs (rate - 1.0) <= 1e-3 || self->bpf == 0 || self->sample_rate == 0) {
    return gst_pad_push (self->srcpad, buf);
  }

  GstMapInfo in_map;
  if (!gst_buffer_map (buf, &in_map, GST_MAP_READ)) {
    return gst_pad_push (self->srcpad, buf);
  }

  gsize bpf = (gsize) self->bpf;
  gsize in_frames = in_map.size / bpf;
  if (in_frames == 0) {
    gst_buffer_unmap (buf, &in_map);
    gst_buffer_unref (buf);
    return GST_FLOW_OK;
  }

  gsize out_frames = (gsize) llround ((double) in_frames / rate);
  if (out_frames == 0)
    out_frames = 1;

  gsize stride_frames = ((gsize) self->sample_rate * (gsize) MAX (self->stride_ms, 8u)) / 1000u;
  if (stride_frames < 32)
    stride_frames = 32;

  GstBuffer *out_buf = gst_buffer_new_allocate (NULL, out_frames * bpf, NULL);
  GstMapInfo out_map;
  if (!gst_buffer_map (out_buf, &out_map, GST_MAP_WRITE)) {
    gst_buffer_unref (out_buf);
    gst_buffer_unmap (buf, &in_map);
    return gst_pad_push (self->srcpad, buf);
  }

  gsize written = 0;
  while (written < out_frames) {
    gsize chunk = MIN (stride_frames, out_frames - written);
    gsize in_start = (gsize) ((double) written * rate);
    if (in_start >= in_frames)
      in_start = in_frames - 1;
    for (gsize f = 0; f < chunk; f++) {
      gsize src_idx = MIN (in_start + f, in_frames - 1);
      memcpy (out_map.data + (written + f) * bpf, in_map.data + src_idx * bpf, bpf);
    }
    written += chunk;
  }

  gst_buffer_unmap (out_buf, &out_map);
  gst_buffer_unmap (buf, &in_map);

  if (GST_BUFFER_PTS_IS_VALID (buf))
    GST_BUFFER_PTS (out_buf) = GST_BUFFER_PTS (buf);
  if (GST_BUFFER_DTS_IS_VALID (buf))
    GST_BUFFER_DTS (out_buf) = GST_BUFFER_DTS (buf);
  GST_BUFFER_DURATION (out_buf) =
      gst_util_uint64_scale_int ((guint64) out_frames, GST_SECOND, (gint) self->sample_rate);

  gst_buffer_unref (buf);
  return gst_pad_push (self->srcpad, out_buf);
}

static void
olaz_scaletempo_init (OlazScaletempo *self)
{
  self->rate = 1.0;
  self->stride_ms = 30;
  self->overlap = 0.2;
  self->search_ms = 14;
  self->bpf = 0;
  self->sample_rate = 0;

  self->sinkpad = gst_pad_new_from_static_template (&olaz_scaletempo_sink_template, "sink");
  gst_pad_set_chain_function (self->sinkpad, GST_DEBUG_FUNCPTR (olaz_scaletempo_chain));
  gst_pad_set_event_function (self->sinkpad, GST_DEBUG_FUNCPTR (olaz_scaletempo_sink_event));
  gst_pad_set_query_function (self->sinkpad, GST_DEBUG_FUNCPTR (olaz_scaletempo_sink_query));
  GST_PAD_SET_PROXY_CAPS (self->sinkpad);
  gst_element_add_pad (GST_ELEMENT (self), self->sinkpad);

  self->srcpad = gst_pad_new_from_static_template (&olaz_scaletempo_src_template, "src");
  GST_PAD_SET_PROXY_CAPS (self->srcpad);
  gst_element_add_pad (GST_ELEMENT (self), self->srcpad);
}

static void
olaz_scaletempo_class_init (OlazScaletempoClass *klass)
{
  GObjectClass *gobject_class = G_OBJECT_CLASS (klass);
  GstElementClass *element_class = GST_ELEMENT_CLASS (klass);

  gobject_class->set_property = olaz_scaletempo_set_property;
  gobject_class->get_property = olaz_scaletempo_get_property;

  g_object_class_install_property (gobject_class, ST_PROP_RATE,
      g_param_spec_double ("rate", "Playback Rate", "Current playback rate",
          G_MINDOUBLE, G_MAXDOUBLE, 1.0, G_PARAM_READABLE | G_PARAM_STATIC_STRINGS));
  g_object_class_install_property (gobject_class, ST_PROP_STRIDE,
      g_param_spec_uint ("stride", "Stride Length", "Length in milliseconds to output each stride",
          1, 5000, 30, G_PARAM_READWRITE | G_PARAM_STATIC_STRINGS));
  g_object_class_install_property (gobject_class, ST_PROP_OVERLAP,
      g_param_spec_double ("overlap", "Overlap Length", "Percentage of stride to overlap",
          0.0, 1.0, 0.2, G_PARAM_READWRITE | G_PARAM_STATIC_STRINGS));
  g_object_class_install_property (gobject_class, ST_PROP_SEARCH,
      g_param_spec_uint ("search", "Search Length", "Length in milliseconds to search for best overlap",
          0, 500, 14, G_PARAM_READWRITE | G_PARAM_STATIC_STRINGS));

  gst_element_class_set_static_metadata (element_class,
      "Olaz Scaletempo Audio Filter",
      "Filter/Effect/Audio",
      "Sync audio tempo with playback rate without shifting pitch",
      "Olaz Contributors");
  gst_element_class_add_static_pad_template (element_class, &olaz_scaletempo_sink_template);
  gst_element_class_add_static_pad_template (element_class, &olaz_scaletempo_src_template);
}

/* Passthrough `deinterlace` fallback so `GstPlaySink` never emits
 * `Missing element 'deinterlace'` warnings when `gst-plugins-good` is not installed. */
static GstStaticPadTemplate olaz_raw_video_sink_tmpl =
GST_STATIC_PAD_TEMPLATE ("sink", GST_PAD_SINK, GST_PAD_ALWAYS, GST_STATIC_CAPS ("video/x-raw(ANY)"));
static GstStaticPadTemplate olaz_raw_video_src_tmpl =
GST_STATIC_PAD_TEMPLATE ("src", GST_PAD_SRC, GST_PAD_ALWAYS, GST_STATIC_CAPS ("video/x-raw(ANY)"));

typedef struct _OlazDeinterlace { GstBaseTransform parent; } OlazDeinterlace;
typedef struct _OlazDeinterlaceClass { GstBaseTransformClass parent_class; } OlazDeinterlaceClass;
GType olaz_deinterlace_get_type (void);
G_DEFINE_TYPE (OlazDeinterlace, olaz_deinterlace, GST_TYPE_BASE_TRANSFORM)
static void olaz_deinterlace_init (OlazDeinterlace *self) {
  gst_base_transform_set_passthrough (GST_BASE_TRANSFORM (self), TRUE);
}
static void olaz_deinterlace_class_init (OlazDeinterlaceClass *klass) {
  GstElementClass *ec = GST_ELEMENT_CLASS (klass);
  gst_element_class_set_static_metadata (ec, "Olaz Deinterlace Passthrough",
      "Filter/Effect/Video/Deinterlace", "Passthrough deinterlace fallback", "Olaz Contributors");
  gst_element_class_add_static_pad_template (ec, &olaz_raw_video_sink_tmpl);
  gst_element_class_add_static_pad_template (ec, &olaz_raw_video_src_tmpl);
}

/* ============================================================================
 * Plugin Entry Point
 * ============================================================================ */

static gboolean
olazauto_plugin_init (GstPlugin *plugin)
{
  gst_plugin_add_dependency_simple (plugin,
      "WEBKIT_GST_ALLOWED_URI_PROTOCOLS:OLAZ_GST_FORCE_AUTO_SINKS:OLAZ_GST_FORCE_SCALETEMPO",
      NULL, NULL, GST_PLUGIN_DEPENDENCY_FLAG_NONE);

  if (!olaz_system_has_autodetect_plugin ()) {
    if (!gst_element_register (plugin, "autoaudiosink", GST_RANK_NONE, OLAZ_TYPE_AUTO_AUDIO_SINK))
      return FALSE;
    if (!gst_element_register (plugin, "autovideosink", GST_RANK_NONE, OLAZ_TYPE_AUTO_VIDEO_SINK))
      return FALSE;
  }

  if (!olaz_system_has_gst_plugin_so ("libgstscaletempoplugin.so", "OLAZ_GST_FORCE_SCALETEMPO")) {
    if (!gst_element_register (plugin, "scaletempo", GST_RANK_PRIMARY, OLAZ_TYPE_SCALETEMPO))
      return FALSE;
  }

  if (!olaz_system_has_gst_plugin_so ("libgstdeinterlace.so", NULL)) {
    if (!gst_element_register (plugin, "deinterlace", GST_RANK_MARGINAL, olaz_deinterlace_get_type ()))
      return FALSE;
  }

  if (!gst_element_register (plugin, "olazwavparse", GST_RANK_MARGINAL, OLAZ_TYPE_WAV_PARSE))
    return FALSE;

#ifdef OLAZ_HAVE_FFMPEG
  if (!olaz_system_has_full_mp4_decoders () || g_getenv ("OLAZ_GST_FORCE_AVDEMUX") != NULL) {
    if (!gst_element_register (plugin, "olazavdemux", GST_RANK_PRIMARY + 10, OLAZ_TYPE_AV_DEMUX))
      return FALSE;
    if (!gst_element_register (plugin, "olazavvideocaps", GST_RANK_MARGINAL, olaz_av_video_caps_get_type ()))
      return FALSE;
    if (!gst_element_register (plugin, "olazavaudiocaps", GST_RANK_MARGINAL, olaz_av_audio_caps_get_type ()))
      return FALSE;
  }
#endif

  if (gst_element_factory_find ("webkitwebsrc") != NULL) {
    GType webkit_sub = olaz_webkit_media_src_get_or_register_subclass ();
    if (webkit_sub != 0) {
      if (!gst_element_register (plugin, "olazmediasrc", GST_RANK_PRIMARY + 100, webkit_sub))
        return FALSE;
      return TRUE;
    }
  }

  if (!gst_element_register (plugin, "olazmediasrc", GST_RANK_PRIMARY + 100,
          olaz_media_src_stub_get_type ())) {
    return FALSE;
  }

  /* If GStreamer scanned plugins in-process inside WebKit before `webkitwebsrc` was registered,
   * register the higher-rank `olazwebkitmediasrc` on the next main-loop tick. */
  if (dlsym (RTLD_DEFAULT, "webkit_settings_get_type") != NULL) {
    g_idle_add_full (G_PRIORITY_HIGH, olaz_register_webkit_media_src_idle_cb, NULL, NULL);
  }

  return TRUE;
}

GST_PLUGIN_DEFINE (
    GST_VERSION_MAJOR,
    GST_VERSION_MINOR,
    olazauto,
    "Olaz fallback auto audio/video sinks and RIFF WAVE parser",
    olazauto_plugin_init,
    "0.1.0",
    "GPL",
    "Olaz",
    "https://github.com/qnguyen3/zalo-swift-re"
)
