#include <stdint.h>
#include <stdlib.h>
#include <string.h>

#include <libavcodec/avcodec.h>
#include <libavfilter/buffersink.h>
#include <libavfilter/buffersrc.h>
#include <libavutil/buffer.h>
#include <libavutil/dict.h>
#include <libavutil/error.h>
#include <libavutil/frame.h>
#include <libavutil/hwcontext.h>
#include <libavutil/imgutils.h>
#include <libavutil/mem.h>
#include <libavutil/opt.h>
#include <libavutil/pixdesc.h>
#include <libavutil/pixfmt.h>

#include "error.h"
#include "log.h"

#ifdef HAS_VAAPI
#include <libavutil/hwcontext_vaapi.h>
#include <va/va.h>
#endif

const AVRational TIME_BASE = (AVRational){1, 1000};
// must match Codec in src/codec/mod.rs (also the wire codec id)
enum { CODEC_H264 = 0, CODEC_HEVC = 1, CODEC_AV1 = 2 };
static const char* const CODEC_LABELS[] = {"H.264", "H.265", "AV1"};
static const char* const VAAPI_ENCODERS[] = {"h264_vaapi", "hevc_vaapi", "av1_vaapi"};
static const char* const NVENC_ENCODERS[] = {"h264_nvenc", "hevc_nvenc", "av1_nvenc"};
// AV1 q index for constant quality, roughly H.264 qp 23
#define AV1_VAAPI_QINDEX 120

typedef struct ScaleContext
{
	AVFilterGraph* filter_graph_scale;
	AVFilterContext* buffersink_scale_ctx;
	AVFilterContext* buffersrc_scale_ctx;
	AVFrame* frame_in;
	AVFrame* frame_out;
} ScaleContext;

typedef struct Scalers
{
	ScaleContext bgr0;
	ScaleContext rgb0;
	ScaleContext rgb;
	AVBufferRef* hw_frames_ctx;
	AVFrame* frame_out;
} Scalers;

typedef struct VideoContext
{
	AVCodecContext* c;

	// pointer to the frame to be encoded, one of frame_out in scalers.bgr0/rgb0/rgb
	AVFrame* frame;

	Scalers scalers;

	AVBufferRef* hw_device_ctx;

	AVPacket* pkt;
	int width_out;
	int height_out;
	int width_in;
	int height_in;
	void* rust_ctx;
	int pts;
	int initialized;
	int frame_allocated;
	int try_vaapi;
	int try_nvenc;
	int bitrate_kbps;
	int frame_rate;
	int codec;
	int force_keyframe;
} VideoContext;

// this is a rust function and lives in src/video.rs; called once per encoded packet (Annex B)
void write_video_packet(
	void* rust_ctx, const uint8_t* buf, int buf_size, int64_t pts_ms, int keyframe);

#if defined(__clang__) || defined(__GNUC__)
void log_callback(__attribute__((unused)) void* _ptr, int level, const char* fmt_orig, va_list args)
#else
void log_callback(void* _ptr, int level, const char* fmt_orig, va_list args)
#endif
{
	char fmt[256] = {0};
	strncpy(fmt, fmt_orig, sizeof(fmt) - 1);
	int done = 0;
	// strip whitespaces from end
	for (int i = sizeof(fmt) - 1; i >= 0 && !done; --i)
		switch (fmt[i])
		{
		case ' ':
		case '\n':
		case '\t':
		case '\r':
			fmt[i] = '\0';
			break;
		case '\0':
			break;
		default:
			done = 1;
		}
	char buf[2048];
	vsnprintf(buf, sizeof(buf), fmt, args);
	switch (level)
	{
	case AV_LOG_FATAL:
	case AV_LOG_ERROR:
	case AV_LOG_PANIC:
		log_error("%s", buf);
		break;
	case AV_LOG_INFO:
		log_info("%s", buf);
		break;
	case AV_LOG_WARNING:
		log_warn("%s", buf);
		break;
	case AV_LOG_QUIET:
		break;
	case AV_LOG_VERBOSE:
		log_debug("%s", buf);
		break;
	case AV_LOG_DEBUG:
		log_trace("%s", buf);
		break;
	}
}

// called in src/log.rs
void init_ffmpeg_logger() { av_log_set_callback(log_callback); }

void set_codec_params(VideoContext* ctx)
{
	/* resolution must be a multiple of two */
	ctx->c->width = ctx->width_out;
	ctx->c->height = ctx->height_out;
	ctx->c->time_base = TIME_BASE;
	ctx->c->framerate = (AVRational){0, 1};
	if (ctx->bitrate_kbps > 0)
	{
		// rate control budgets bits per frame; without a frame rate it assumes 1/time_base = 1000 fps
		ctx->c->framerate = (AVRational){ctx->frame_rate > 0 ? ctx->frame_rate : 30, 1};
		ctx->c->rc_max_rate = (int64_t)ctx->bitrate_kbps * 1000;
		// 0.5 s of VBV: bounds how far a single frame can exceed the average (transfer latency)
		ctx->c->rc_buffer_size = ctx->bitrate_kbps * 500;
	}

	ctx->c->gop_size = 12;
	// no B-frames to reduce latency
	ctx->c->max_b_frames = 0;
	// no global header: encoders then repeat SPS/PPS (VPS for HEVC) or the AV1 sequence header
	// in-band before every keyframe (read by src/codec/)
}

void destroy_scale_ctx(ScaleContext* ctx)
{
	avfilter_graph_free(&ctx->filter_graph_scale);
	if (ctx->frame_in)
		av_frame_free(&ctx->frame_in);
}

void init_scaler(
	ScaleContext* ctx,
	int width_in,
	int height_in,
	int width_out,
	int height_out,
	enum AVPixelFormat pix_fmt_in,
	enum AVPixelFormat pix_fmt_out,
	AVBufferRef* hw_device_ctx,
	enum AVPixelFormat pix_fmt_sw_out,
	AVFrame* frame_out,
	Error* err)
{
	int ret = 0;

	ctx->frame_in = av_frame_alloc();
	if (!ctx->frame_in)
		ERROR(err, 1, "Failed to allocate frame_in for scale filter!");

	ctx->frame_out = frame_out;

	ctx->frame_in->format = pix_fmt_in;
	ctx->frame_in->width = width_in;
	ctx->frame_in->height = height_in;
	ret = av_frame_get_buffer(ctx->frame_in, 0);
	if (ret)
	{
		destroy_scale_ctx(ctx);
		ERROR(
			err,
			1,
			"Failed to allocate buffer for frame_in for scale filter: %s!",
			av_err2str(ret));
	}

	char args[512];
	const AVFilter* buffersrc = avfilter_get_by_name("buffer");
	const AVFilter* buffersink = avfilter_get_by_name("buffersink");
	AVFilterInOut* outputs = NULL;
	AVFilterInOut* inputs = NULL;
	AVFilterGraphSegment* seg = NULL;

	ctx->filter_graph_scale = avfilter_graph_alloc();
	if (!ctx->filter_graph_scale)
	{
		ret = AVERROR(ENOMEM);
		goto end;
	}

	avfilter_graph_set_auto_convert(ctx->filter_graph_scale, AVFILTER_AUTO_CONVERT_NONE);

	/* buffer video source: the decoded frames from the decoder will be inserted here. */
	snprintf(
		args,
		sizeof(args),
		"video_size=%dx%d:pix_fmt=%d:time_base=%d/%d:pixel_aspect=%d/%d",
		width_in,
		height_in,
		pix_fmt_in,
		TIME_BASE.num,
		TIME_BASE.den,
		1,
		1);

	ret = avfilter_graph_create_filter(
		&ctx->buffersrc_scale_ctx, buffersrc, "in", args, NULL, ctx->filter_graph_scale);
	if (ret < 0)
	{
		log_warn("Cannot create buffer source");
		goto end;
	}

	/* buffer video sink: to terminate the filter chain. */
	ctx->buffersink_scale_ctx =
		avfilter_graph_alloc_filter(ctx->filter_graph_scale, buffersink, "out");

	if (ctx->buffersink_scale_ctx == NULL)
	{
		log_warn("Cannot allocate buffer sink");
		goto end;
	}

#if LIBAVUTIL_VERSION_INT >= AV_VERSION_INT(59, 39, 100) && \
	LIBAVFILTER_VERSION_INT >= AV_VERSION_INT(10, 6, 100)
	ret = av_opt_set_array(
		ctx->buffersink_scale_ctx,
		"pixel_formats",
		AV_OPT_SEARCH_CHILDREN,
		0,
		1,
		AV_OPT_TYPE_PIXEL_FMT,
		&pix_fmt_out);
#else
	// libavfilter < 10.6 (FFmpeg 7.1 and older): only the binary "pix_fmts" option exists (removed in FFmpeg 8).
	enum AVPixelFormat sink_pix_fmts[] = {pix_fmt_out, AV_PIX_FMT_NONE};
	ret = av_opt_set_int_list(
		ctx->buffersink_scale_ctx,
		"pix_fmts",
		sink_pix_fmts,
		AV_PIX_FMT_NONE,
		AV_OPT_SEARCH_CHILDREN);
#endif
	if (ret < 0)
	{
		log_warn("Cannot set output pixel format: %s", av_err2str(ret));
		goto end;
	}

	ret = avfilter_init_dict(ctx->buffersink_scale_ctx, NULL);
	if (ret < 0)
	{
		log_warn("Cannot init buffer sink");
		goto end;
	}

	switch (pix_fmt_out)
	{
	case AV_PIX_FMT_CUDA:
		if (pix_fmt_in == AV_PIX_FMT_RGB24)
		{
			snprintf(
				args,
				sizeof(args),
				"scale=w=%d:h=%d:flags=fast_bilinear,hwupload_cuda",
				width_out,
				height_out);
		}
		else
		{
			snprintf(
				args,
				sizeof(args),
				"hwupload_cuda,scale_cuda=w=%d:h=%d:format=%s:interp_algo=nearest",
				width_out,
				height_out,
				av_get_pix_fmt_name(pix_fmt_sw_out));
		}
		break;
	case AV_PIX_FMT_VAAPI:
		if (pix_fmt_in == AV_PIX_FMT_RGB24)
			snprintf(
				args,
				sizeof(args),
				"scale=w=%d:h=%d:flags=fast_bilinear,hwupload",
				width_out,
				height_out);
		else
			snprintf(
				args,
				sizeof(args),
				"hwupload,scale_vaapi=w=%d:h=%d:format=%s:mode=fast",
				width_out,
				height_out,
				av_get_pix_fmt_name(pix_fmt_sw_out));
		break;
	default:
		snprintf(args, sizeof(args), "scale=w=%d:h=%d:flags=fast_bilinear", width_out, height_out);
	}

	// hwupload reads hw_device_ctx during init and avfilter_graph_parse_ptr inits immediately,
	// so create the filters, then set the device, then init.
	ret = avfilter_graph_segment_parse(ctx->filter_graph_scale, args, 0, &seg);
	if (ret < 0)
	{
		log_warn("Failed to parse filter: %s", av_err2str(ret));
		goto end;
	}

	ret = avfilter_graph_segment_create_filters(seg, 0);
	if (ret < 0)
	{
		log_warn("Failed to create filters: %s", av_err2str(ret));
		goto end;
	}

	for (size_t c = 0; c < seg->nb_chains; c++)
	{
		for (size_t f = 0; f < seg->chains[c]->nb_filters; f++)
		{
			AVFilterContext* filt = seg->chains[c]->filters[f]->filter;
			if (hw_device_ctx && filt && strcmp(filt->filter->name, "hwupload") == 0)
				filt->hw_device_ctx = av_buffer_ref(hw_device_ctx);
		}
	}

	ret = avfilter_graph_segment_apply(seg, 0, &inputs, &outputs);
	if (ret < 0)
	{
		log_warn("Failed to apply filter graph segment: %s", av_err2str(ret));
		goto end;
	}

	if (!inputs || inputs->next || !outputs || outputs->next)
	{
		ret = AVERROR(EINVAL);
		log_warn("Unexpected filter graph endpoints");
		goto end;
	}

	ret = avfilter_link(ctx->buffersrc_scale_ctx, 0, inputs->filter_ctx, inputs->pad_idx);
	if (ret < 0)
	{
		log_warn("Failed to link buffer source: %s", av_err2str(ret));
		goto end;
	}

	ret = avfilter_link(outputs->filter_ctx, outputs->pad_idx, ctx->buffersink_scale_ctx, 0);
	if (ret < 0)
	{
		log_warn("Failed to link buffer sink: %s", av_err2str(ret));
		goto end;
	}

	if ((ret = avfilter_graph_config(ctx->filter_graph_scale, NULL)) < 0)
	{
		log_warn("Failed to configure filter graph");
		goto end;
	}

end:
	avfilter_graph_segment_free(&seg);
	avfilter_inout_free(&inputs);
	avfilter_inout_free(&outputs);

	if (ret != 0)
	{
		destroy_scale_ctx(ctx);
		ERROR(
			err,
			1,
			"Setting up scale filter %s -> %s (sw: %s) failed!",
			av_get_pix_fmt_name(pix_fmt_in),
			av_get_pix_fmt_name(pix_fmt_out),
			av_get_pix_fmt_name(pix_fmt_sw_out));
	}
	else
	{
		log_debug(
			"Scale filter set %s -> %s (sw: %s) up!",
			av_get_pix_fmt_name(pix_fmt_in),
			av_get_pix_fmt_name(pix_fmt_out),
			av_get_pix_fmt_name(pix_fmt_sw_out));
	}
}

void destroy_scalers(Scalers* s)
{
	destroy_scale_ctx(&s->bgr0);
	destroy_scale_ctx(&s->rgb0);
	destroy_scale_ctx(&s->rgb);
	if (s->frame_out)
		av_frame_free(&s->frame_out);
}

void init_scalers(
	Scalers* ctx,
	int width_in,
	int height_in,
	int width_out,
	int height_out,
	enum AVPixelFormat pix_fmt_out,
	enum AVPixelFormat pix_fmt_sw_out,
	AVBufferRef* hw_device_ctx,
	Error* err)
{
	int ret;
	ctx->frame_out = av_frame_alloc();
	if (!ctx->frame_out)
	{
		destroy_scalers(ctx);
		ERROR(err, 1, "Failed to allocate frame_out for scale filter!");
	}

	if (hw_device_ctx != NULL)
	{

		AVBufferRef* hw_frames_ref;
		AVHWFramesContext* frames_ctx = NULL;
		if (!(hw_frames_ref = av_hwframe_ctx_alloc(hw_device_ctx)))
		{
			destroy_scalers(ctx);
			ERROR(err, 1, "Failed to create HW frame context.");
		}
		frames_ctx = (AVHWFramesContext*)(hw_frames_ref->data);
		frames_ctx->format = pix_fmt_out;
		frames_ctx->sw_format = pix_fmt_sw_out;
		frames_ctx->width = width_out;
		frames_ctx->height = height_out;
		frames_ctx->initial_pool_size = 20;
		if ((ret = av_hwframe_ctx_init(hw_frames_ref)) < 0)
		{
			av_buffer_unref(&hw_frames_ref);
			destroy_scalers(ctx);
			ERROR(
				err,
				1,
				"Failed to initialize HW frame context."
				"Error code: %s",
				av_err2str(ret));
		}

		ctx->hw_frames_ctx = av_buffer_ref(hw_frames_ref);
		ret = av_hwframe_get_buffer(ctx->hw_frames_ctx, ctx->frame_out, 0);
		if (ret < 0)
		{
			av_buffer_unref(&hw_frames_ref);
			destroy_scalers(ctx);
			ERROR(
				err,
				1,
				"Could not allocate video hardware frame data for scaling: %s",
				av_err2str(ret));
		}
		av_buffer_unref(&hw_frames_ref);
	}

	enum AVPixelFormat pix_fmts[] = {AV_PIX_FMT_BGR0, AV_PIX_FMT_RGB0, AV_PIX_FMT_RGB24};
	ScaleContext* scalers[] = {&ctx->bgr0, &ctx->rgb0, &ctx->rgb};
	for (int i = 0; i < 3; i++)
	{
		init_scaler(
			scalers[i],
			width_in,
			height_in,
			width_out,
			height_out,
			pix_fmts[i],
			pix_fmt_out,
			hw_device_ctx,
			pix_fmt_sw_out,
			ctx->frame_out,
			err);
		OK_OR_ABORT(err);
	}
}

void scale_frame(ScaleContext* ctx, Error* err)
{
	int ret;
	if ((ret = av_buffersrc_add_frame_flags(
				   ctx->buffersrc_scale_ctx, ctx->frame_in, AV_BUFFERSRC_FLAG_KEEP_REF) < 0))
	{
		ERROR(err, ret, "Error adding frame to buffer source: %s.", av_err2str(ret));
	}

	av_frame_unref(ctx->frame_out);

	while (1)
	{
		int ret = av_buffersink_get_frame(ctx->buffersink_scale_ctx, ctx->frame_out);
		if (ret == AVERROR(EAGAIN) || ret == AVERROR_EOF)
			break;
		if (ret < 0)
		{
			ERROR(err, ret, "Error reading frame from buffer sink: %s.", av_err2str(ret));
		}
	}
}

void open_video(VideoContext* ctx, Error* err)
{
	if (ctx->codec < CODEC_H264 || ctx->codec > CODEC_AV1)
		ERROR(err, 1, "Invalid codec id: %d", ctx->codec);

	if (ctx->width_out <= 1 || ctx->height_out <= 1)
		ERROR(
			err,
			1,
			"Invalid size for video: width = %d, height = %d",
			ctx->width_out,
			ctx->height_out);

	const AVCodec* codec;
	int ret;

	int using_hw = 0;

#ifdef HAS_VAAPI
	char* vaapi_device = getenv("WEYLUS_VAAPI_DEVICE");

	if (ctx->try_vaapi &&
		av_hwdevice_ctx_create(
			&ctx->hw_device_ctx, AV_HWDEVICE_TYPE_VAAPI, vaapi_device, NULL, 0) == 0)
	{

		if (ctx->hw_device_ctx)
		{
			AVHWFramesConstraints* cst =
				av_hwdevice_get_hwframe_constraints(ctx->hw_device_ctx, NULL);
			if (cst)
			{
				for (enum AVPixelFormat* fmt = cst->valid_sw_formats; *fmt != AV_PIX_FMT_NONE;
					 ++fmt)
				{
					log_debug("VAAPI: valid pix_fmt: %s", av_get_pix_fmt_name(*fmt));
				}
				av_hwframe_constraints_free(&cst);
			}
		}

		codec = avcodec_find_encoder_by_name(VAAPI_ENCODERS[ctx->codec]);
		if (codec)
		{
			ctx->c = avcodec_alloc_context3(codec);
			if (ctx->c)
			{
				Error err = {0};
				init_scalers(
					&ctx->scalers,
					ctx->width_in,
					ctx->height_in,
					ctx->width_out,
					ctx->height_out,
					AV_PIX_FMT_VAAPI,
					AV_PIX_FMT_NV12,
					ctx->hw_device_ctx,
					&err);
				if (err.code)
				{
					log_warn("VAAPI: failed to initialize scaler: %s", err.error_str);
					avcodec_free_context(&ctx->c);
				}
				else
				{
					ctx->c->pix_fmt = AV_PIX_FMT_VAAPI;
					ctx->c->hw_frames_ctx = ctx->scalers.hw_frames_ctx;
					av_opt_set(ctx->c->priv_data, "quality", "7", 0);
					if (ctx->bitrate_kbps > 0)
					{
						av_opt_set(ctx->c->priv_data, "rc_mode", "VBR", 0);
						ctx->c->bit_rate = (int64_t)ctx->bitrate_kbps * 1000;
					}
					else if (ctx->codec == CODEC_AV1)
					{
						// av1_vaapi has no qp option; CQP takes base_q_idx (0-255) from global_quality
						av_opt_set(ctx->c->priv_data, "rc_mode", "CQP", 0);
						ctx->c->global_quality = AV1_VAAPI_QINDEX;
					}
					else
						av_opt_set(ctx->c->priv_data, "qp", "23", 0);
					// default 2 holds a finished packet until more frames arrive; frames only come
					// on screen damage, so that adds a random delay
					av_opt_set(ctx->c->priv_data, "async_depth", "1", 0);
					set_codec_params(ctx);

					if ((ret = avcodec_open2(ctx->c, codec, NULL)) == 0)
						using_hw = 1;
					else
					{
						log_warn("VAAPI: could not open codec: %s!", av_err2str(ret));
						avcodec_free_context(&ctx->c);
						av_buffer_unref(&ctx->hw_device_ctx);
						destroy_scalers(&ctx->scalers);
					}
				}
			}
		}
		else
			av_buffer_unref(&ctx->hw_device_ctx);
	}
#endif


#ifdef HAS_NVENC
	if (ctx->try_nvenc && !using_hw &&
		av_hwdevice_ctx_create(&ctx->hw_device_ctx, AV_HWDEVICE_TYPE_CUDA, NULL, NULL, 0) == 0)
	{
		codec = avcodec_find_encoder_by_name(NVENC_ENCODERS[ctx->codec]);
		if (codec)
		{
			ctx->c = avcodec_alloc_context3(codec);
			if (ctx->c)
			{
				Error err = {0};
				init_scalers(
					&ctx->scalers,
					ctx->width_in,
					ctx->height_in,
					ctx->width_out,
					ctx->height_out,
					AV_PIX_FMT_CUDA,
					AV_PIX_FMT_BGR0,
					ctx->hw_device_ctx,
					&err);
				if (err.code)
				{
					log_warn("Failed to initialize scaler: %s", err.error_str);
					avcodec_free_context(&ctx->c);
				}
				else
				{
					ctx->c->pix_fmt = AV_PIX_FMT_CUDA;
					ctx->c->hw_frames_ctx = ctx->scalers.hw_frames_ctx;
					av_opt_set(ctx->c->priv_data, "preset", "p1", 0);
					av_opt_set(ctx->c->priv_data, "zerolatency", "1", 0);
					av_opt_set(ctx->c->priv_data, "tune", "ull", 0);
					av_opt_set(ctx->c->priv_data, "rc", "cbr", 0);
					av_opt_set(ctx->c->priv_data, "cq", "21", 0);
					av_opt_set(ctx->c->priv_data, "delay", "0", 0);
					av_opt_set(ctx->c->priv_data, "forced-idr", "1", 0);
					if (ctx->bitrate_kbps > 0)
						ctx->c->bit_rate = (int64_t)ctx->bitrate_kbps * 1000;
					set_codec_params(ctx);

					int ret = avcodec_open2(ctx->c, codec, NULL);
					if (ret == 0)
						using_hw = 1;
					else
					{
						log_debug("Could not open codec: %s!", av_err2str(ret));
						avcodec_free_context(&ctx->c);
						destroy_scalers(&ctx->scalers);
					}
				}
			}
			else
				log_debug("Could not allocate video codec context for '%s'!", NVENC_ENCODERS[ctx->codec]);
		}
		else
			log_debug("Codec '%s' not found!", NVENC_ENCODERS[ctx->codec]);
	}
#endif


	// HEVC/AV1 are hardware-only; src/video.rs then tries the client's next codec (H.264 last)
	if (!using_hw && ctx->codec != CODEC_H264)
		ERROR(err, 1, "No hardware encoder for %s could be opened", CODEC_LABELS[ctx->codec]);

	if (!using_hw)
	{
		codec = avcodec_find_encoder_by_name("libx264");
		if (!codec)
		{
			ERROR(err, 1, "Codec 'libx264' not found");
		}

		ctx->c = avcodec_alloc_context3(codec);
		if (!ctx->c)
		{
			ERROR(err, 1, "Could not allocate video codec context");
		}

		init_scalers(
			&ctx->scalers,
			ctx->width_in,
			ctx->height_in,
			ctx->width_out,
			ctx->height_out,
			AV_PIX_FMT_YUV420P,
			AV_PIX_FMT_YUV420P,
			NULL,
			err);
		if (err->code)
		{
			avcodec_free_context(&ctx->c);
			return;
		}

		ctx->c->pix_fmt = AV_PIX_FMT_YUV420P;
		av_opt_set(ctx->c->priv_data, "preset", "ultrafast", 0);
		av_opt_set(ctx->c->priv_data, "tune", "zerolatency", 0);
		av_opt_set(ctx->c->priv_data, "crf", "23", 0);
		av_opt_set(ctx->c->priv_data, "forced-idr", "1", 0);
		set_codec_params(ctx);

		ret = avcodec_open2(ctx->c, codec, NULL);
		if (ret < 0)
		{
			avcodec_free_context(&ctx->c);
			ERROR(err, 1, "Could not open codec: %s", av_err2str(ret));
		}
	}

	ctx->pkt = av_packet_alloc();
	if (!ctx->pkt)
		ERROR(err, 1, "Failed to allocate packet");

	if (av_pix_fmt_desc_get(ctx->c->pix_fmt)->flags & AV_PIX_FMT_FLAG_HWACCEL &&
		ctx->c->hw_frames_ctx)
	{
		const char* pix_fmt_sw =
			av_get_pix_fmt_name(((AVHWFramesContext*)ctx->c->hw_frames_ctx->data)->sw_format);
		log_info(
			"Video: %dx%d@%s pix_fmt: %s (%s)",
			ctx->width_out,
			ctx->height_out,
			ctx->c->codec->name,
			av_get_pix_fmt_name(ctx->c->pix_fmt),
			pix_fmt_sw);
	}
	else
		log_info(
			"Video: %dx%d@%s pix_fmt: %s",
			ctx->width_out,
			ctx->height_out,
			ctx->c->codec->name,
			av_get_pix_fmt_name(ctx->c->pix_fmt));

	ctx->initialized = 1;
}

void destroy_video_encoder(VideoContext* ctx)
{
	if (ctx->initialized)
	{
		avcodec_free_context(&ctx->c);
		av_packet_free(&ctx->pkt);
		destroy_scalers(&ctx->scalers);
	}
	if (ctx->hw_device_ctx)
		av_buffer_unref(&ctx->hw_device_ctx);
	free(ctx);
}

void encode_video_frame(VideoContext* ctx, int millis, Error* err)
{
	int ret;
	AVFrame* frame = ctx->frame;
	if (!frame)
		ERROR(err, 1, "Frame not initialized!");

	frame->pts = millis;
	// the scaler reuses its output frame, so the picture type must be reset every time
	frame->pict_type = ctx->force_keyframe ? AV_PICTURE_TYPE_I : AV_PICTURE_TYPE_NONE;
	ctx->force_keyframe = 0;

	ret = avcodec_send_frame(ctx->c, frame);
	if (ret < 0)
		ERROR(err, 1, "Error sending a frame for encoding: %s", av_err2str(ret));

	while (ret >= 0)
	{
		ret = avcodec_receive_packet(ctx->c, ctx->pkt);
		if (ret == AVERROR(EAGAIN) || ret == AVERROR_EOF)
			return;
		else if (ret < 0)
		{
			ERROR(err, 1, "Error during encoding");
		}

		write_video_packet(
			ctx->rust_ctx,
			ctx->pkt->data,
			ctx->pkt->size,
			ctx->pkt->pts,
			(ctx->pkt->flags & AV_PKT_FLAG_KEY) != 0);
		av_packet_unref(ctx->pkt);
	}
}

void request_keyframe(VideoContext* ctx) { ctx->force_keyframe = 1; }

const char* get_codec_name(VideoContext* ctx)
{
	return ctx->c && ctx->c->codec ? ctx->c->codec->name : NULL;
}

VideoContext* init_video_encoder(
	void* rust_ctx,
	int width_in,
	int height_in,
	int width_out,
	int height_out,
	int try_vaapi,
	int try_nvenc,
	int bitrate_kbps,
	int frame_rate,
	int codec)
{
	VideoContext* ctx = malloc(sizeof(VideoContext));
	ctx->rust_ctx = rust_ctx;
	ctx->width_out = width_out - width_out % 2;
	ctx->height_out = height_out - height_out % 2;
	ctx->width_in = width_in;
	ctx->height_in = height_in;
	ctx->pts = 0;
	ctx->initialized = 0;
	ctx->frame_allocated = 0;
	ctx->try_vaapi = try_vaapi;
	ctx->try_nvenc = try_nvenc;
	ctx->bitrate_kbps = bitrate_kbps;
	ctx->frame_rate = frame_rate;
	ctx->codec = codec;
	ctx->force_keyframe = 0;
	ctx->hw_device_ctx = NULL;

	// make sure all scalers are zero initialized so that destroy can always be called
	memset(&ctx->scalers, 0, sizeof(Scalers));
	return ctx;
}

void fill_bgr0(VideoContext* ctx, const void* data, int stride, Error* err)
{
	ctx->frame = NULL;
	ScaleContext* scaler = &ctx->scalers.bgr0;
	scaler->frame_in->data[0] = (uint8_t*)data;
	scaler->frame_in->linesize[0] = stride;

	scale_frame(scaler, err);
	OK_OR_ABORT(err)
	ctx->frame = scaler->frame_out;
}

void fill_rgb(VideoContext* ctx, const void* data, Error* err)
{
	ctx->frame = NULL;
	ScaleContext* scaler = &ctx->scalers.rgb;
	ctx->frame = NULL;
	scaler->frame_in->data[0] = (uint8_t*)data;
	scaler->frame_in->linesize[0] = ctx->width_in * 3;

	scale_frame(scaler, err);
	OK_OR_ABORT(err)
	ctx->frame = scaler->frame_out;
}

void fill_rgb0(VideoContext* ctx, const void* data, Error* err)
{
	ctx->frame = NULL;
	ScaleContext* scaler = &ctx->scalers.rgb0;
	scaler->frame_in->data[0] = (uint8_t*)data;
	scaler->frame_in->linesize[0] = ctx->width_in * 4;

	scale_frame(scaler, err);
	OK_OR_ABORT(err)
	ctx->frame = scaler->frame_out;
}
