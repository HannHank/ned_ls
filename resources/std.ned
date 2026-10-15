// Built-in types that are implicitly available in every NED file.
// The declarations mirror the built-in channel types of OMNeT++ (https://omnetpp.org);
// the documentation comments are written for ned_ls. See the OMNeT++ Simulation
// Manual.

package ned;

// Standard Channels

//
// Passes messages through instantly and unchanged. It has no parameters.
//
// Using it is equivalent to a connection without any channel. It is mainly
// useful when a connection needs a channel object, e.g. to attach custom
// properties or parameters to it.
//
channel IdealChannel {
    @class(cIdealChannel);
}

//
// Adds a fixed propagation delay to every message.
//
// Parameters:
// - `delay`: propagation delay, with a time unit (e.g. `10ms`). Defaults to `0s`.
// - `disabled`: if `true`, every message sent through the channel is dropped.
//
channel DelayChannel {
    @class(cDelayChannel);
    bool disabled = default(false);
    double delay = default(0s) @unit(s);
}

//
// Like `DelayChannel`, but additionally models transmission time and bit/packet errors.
//
// Parameters:
// - `delay`, `disabled`: same as in `DelayChannel`.
// - `datarate`: link speed in bit/s units (e.g. `100Mbps`). `0` means unlimited,
//   i.e. transmission takes no time.
// - `ber`: probability of a single bit being corrupted, between 0 and 1.
// - `per`: probability of a whole packet being corrupted, between 0 and 1.
//
// A corrupted packet is still delivered, but with its error flag set. It is up to
// the receiving module to check the flag and drop the packet.
//
channel DatarateChannel {
    @class(cDatarateChannel);
    bool disabled = default(false);
    double delay = default(0s) @unit(s);
    double datarate = default(0bps) @unit(bps);
    double ber = default(0);
    double per = default(0);
}
