use crate::Frame;

pub enum SessionEvent {
    // FrameIn is the only message from dispatcher -> Session. There
    // is bunch of other variants pending to be added here to support
    // the communication from application -> socket -> session. See
    // https://github.com/chelmiki/uax25/issues/4
    FrameIn(Frame),
}
