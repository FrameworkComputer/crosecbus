#include <evntrace.h>

#ifndef _TRACE_H_
#define _TRACE_H_

	//
	// Tracing Definitions:
	//
	// Control GUID: 
	// {73e3b785-f5fb-423e-94a9-56627fea9053}

#define WPP_CONTROL_GUIDS                           \
	WPP_DEFINE_CONTROL_GUID(                        \
		CrosECTraceGuid,                            \
		(73e3b785, f5fb, 423e, 94a9, 56627fea9053), \
	    WPP_DEFINE_BIT(CROSEC_ALL_INFO)             \
		WPP_DEFINE_BIT(TRACE_CROSECBUS)             \
		WPP_DEFINE_BIT(TRACE_COMM_LPC)              \
		WPP_DEFINE_BIT(TRACE_COMM_MEC_LPC)          \
		WPP_DEFINE_BIT(TRACE_USERSPACEQUEUE)        \
		)

#define WPP_FLAG_LEVEL_LOGGER(flag, level) WPP_LEVEL_LOGGER(flag)

#define WPP_FLAG_LEVEL_ENABLED(flag, level) (WPP_LEVEL_ENABLED(flag) && WPP_CONTROL(WPP_BIT_##flag).Level >= level)

#define WPP_LEVEL_FLAGS_LOGGER(lvl, flags) WPP_LEVEL_LOGGER(flags)

#define WPP_LEVEL_FLAGS_ENABLED(lvl, flags) (WPP_LEVEL_ENABLED(flags) && WPP_CONTROL(WPP_BIT_##flags).Level >= lvl)

//
// WPP orders static parameters before dynamic parameters. To support the Trace function
// defined below which sets FLAGS=CROSEC_ALL_INFO, a custom macro must be defined to
// reorder the arguments to what the .tpl configuration file expects.
//
#define WPP_RECORDER_FLAGS_LEVEL_ARGS(flags, lvl)   WPP_RECORDER_LEVEL_FLAGS_ARGS(lvl, flags)
#define WPP_RECORDER_FLAGS_LEVEL_FILTER(flags, lvl) WPP_RECORDER_LEVEL_FLAGS_FILTER(lvl, flags)

//
// This comment block is scanned by the trace preprocessor to define our
// Trace function.
//
// begin_wpp config
// FUNC Trace{FLAGS=CROSEC_ALL_INFO}(LEVEL, MSG, ...);
// FUNC TraceEvents(LEVEL, FLAGS, MSG, ...);
// end_wpp
//


#endif _TRACE_H_
