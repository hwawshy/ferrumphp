use crate::php::Job;
use bytes::Bytes;
use futures_util::Stream;
use http_body_util::StreamBody;
use hyper::Error as HyperError;
use hyper::body::{Body, Frame, Incoming};
use hyper::http::response::Parts;
use hyper::{Request, Response};
use std::net::SocketAddr;
use std::pin::Pin;
use std::task::{Context, Poll, ready};
use thiserror::Error;
use tokio::sync::mpsc::{Receiver, Sender, channel};
use tokio::sync::oneshot;
use tokio::sync::oneshot::error::RecvError;
use tokio_stream::wrappers::ReceiverStream;
use tokio_util::sync::PollSender;
use tower::Service;

#[derive(Clone)]
pub struct PhpService {
    sender: PollSender<Job>,
    peer_addr: SocketAddr,
}

impl PhpService {
    pub fn new(sender: Sender<Job>, peer_addr: SocketAddr) -> Self {
        Self {
            sender: PollSender::new(sender),
            peer_addr,
        }
    }
}

#[derive(Error, Debug)]
pub enum PhpError {
    #[error("hyper transport error: {0}")]
    Hyper(#[from] HyperError),

    #[error("failed to enqueue job to worker")]
    JobChannelClosed,

    #[error("worker request body channel closed")]
    RequestBodyClosed,

    #[error("worker response header channel closed")]
    ResponseHeadClosed(#[from] RecvError),
}

impl Service<Request<Incoming>> for PhpService {
    type Response = Response<StreamBody<PhpStream>>;
    type Error = PhpError;
    type Future = PhpFuture;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.sender
            .poll_reserve(cx)
            .map_err(|_| PhpError::JobChannelClosed)
    }

    fn call(&mut self, mut req: Request<Incoming>) -> Self::Future {
        req.extensions_mut().insert(self.peer_addr);

        let (parts, body) = req.into_parts();

        let (response_body_tx, response_body_rx) = channel::<Bytes>(10); // @todo rethink this buffer
        let (response_head_tx, response_head_rx) = oneshot::channel::<Parts>();

        let span = tracing::Span::current();

        // Do we have a request body?
        let (job, fut) = if body.is_end_stream() {
            let job = Job {
                span,
                request_head: parts,
                request_body_rx: None,
                response_head_tx,
                response_body_tx,
            };

            let fut = PhpFuture::WithoutRequestBody {
                response_body_rx,
                response_head_rx,
            };

            (job, fut)
        } else {
            // This buffer plays a role in ensuring fairness cap per poll in RequestBodyFuture
            let (request_body_tx, request_body_rx) = channel::<Bytes>(8); // @todo rethink this buffer

            let job = Job {
                span,
                request_head: parts,
                request_body_rx: Some(request_body_rx),
                response_head_tx,
                response_body_tx,
            };

            let request_body_future = RequestBodyFuture {
                request_body: body,
                request_body_tx: PollSender::new(request_body_tx),
                current_request_body_chunk: None,
            };

            let fut = PhpFuture::WithRequestBody {
                request_body_future,
                response_body_rx,
                response_head_rx,
            };

            (job, fut)
        };

        if self.sender.send_item(job).is_err() {
            return PhpFuture::Err(PhpError::JobChannelClosed);
        };

        fut
    }
}

pub struct RequestBodyFuture {
    request_body: Incoming,
    request_body_tx: PollSender<Bytes>,
    current_request_body_chunk: Option<Bytes>,
}

impl Future for RequestBodyFuture {
    type Output = Result<(), PhpError>;

    fn poll(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        loop {
            match self.current_request_body_chunk {
                None => {
                    let frame = ready!(Pin::new(&mut self.request_body).poll_frame(cx));

                    if let Some(frame) = frame {
                        // ignore trailers
                        if let Ok(data) = frame?.into_data() {
                            self.current_request_body_chunk = Some(data);
                        }
                    } else {
                        // end of stream
                        self.request_body_tx.close();
                        return Poll::Ready(Ok(()));
                    }
                }
                Some(_) => {
                    // stream body chunk
                    ready!(Pin::new(&mut self.request_body_tx).poll_reserve(cx))
                        .map_err(|_| PhpError::RequestBodyClosed)?;

                    let chunk = self.current_request_body_chunk.take().unwrap();
                    self.request_body_tx
                        .send_item(chunk)
                        .map_err(|_| PhpError::RequestBodyClosed)?
                }
            }
        }
    }
}

pub enum PhpFuture {
    WithRequestBody {
        request_body_future: RequestBodyFuture,
        response_head_rx: oneshot::Receiver<Parts>,
        response_body_rx: Receiver<Bytes>,
    },
    WithoutRequestBody {
        response_head_rx: oneshot::Receiver<Parts>,
        response_body_rx: Receiver<Bytes>,
    },
    Err(PhpError),
    Done,
}

impl PhpFuture {
    fn transition_to_without_request_body(&mut self) {
        match std::mem::replace(self, PhpFuture::Done) {
            PhpFuture::WithRequestBody {
                response_head_rx,
                response_body_rx,
                ..
            } => {
                *self = PhpFuture::WithoutRequestBody {
                    response_head_rx,
                    response_body_rx,
                }
            }
            _ => unreachable!(),
        }
    }

    fn transition_to_done(&mut self) -> Self {
        std::mem::replace(self, PhpFuture::Done)
    }
}

impl Future for PhpFuture {
    type Output = Result<Response<StreamBody<PhpStream>>, PhpError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let this = self.get_mut();
        match this {
            PhpFuture::Done => panic!("PhpFuture polled after completion"),
            PhpFuture::Err(_) => match this.transition_to_done() {
                PhpFuture::Err(e) => Poll::Ready(Err(e)),
                _ => unreachable!(),
            },
            PhpFuture::WithoutRequestBody {
                response_head_rx, ..
            } => {
                let parts = ready!(Pin::new(response_head_rx).poll(cx))
                    .map_err(PhpError::ResponseHeadClosed)?;

                match this.transition_to_done() {
                    PhpFuture::WithoutRequestBody {
                        response_body_rx, ..
                    } => {
                        let stream = PhpStream::WithoutRequestBody {
                            response_body_stream: ReceiverStream::new(response_body_rx),
                        };

                        Poll::Ready(Ok(Response::from_parts(parts, StreamBody::new(stream))))
                    }
                    _ => unreachable!(),
                }
            }
            PhpFuture::WithRequestBody {
                response_head_rx,
                request_body_future,
                ..
            } => {
                // First see if PHP headers are ready
                if let Poll::Ready(parts) = Pin::new(response_head_rx).poll(cx)? {
                    match this.transition_to_done() {
                        PhpFuture::WithRequestBody {
                            request_body_future,
                            response_body_rx,
                            ..
                        } => {
                            let stream = PhpStream::WithRequestBody {
                                request_body_future,
                                response_body_stream: ReceiverStream::new(response_body_rx),
                            };

                            return Poll::Ready(Ok(Response::from_parts(
                                parts,
                                StreamBody::new(stream),
                            )));
                        }
                        _ => unreachable!(),
                    }
                }

                // Headers not ready, try streaming request to PHP
                match Pin::new(request_body_future).poll(cx) {
                    Poll::Pending => Poll::Pending,
                    Poll::Ready(Ok(_)) => {
                        this.transition_to_without_request_body();
                        Poll::Pending
                    }
                    Poll::Ready(Err(e)) => Poll::Ready(Err(e)),
                }
            }
        }
    }
}

pub enum PhpStream {
    WithRequestBody {
        request_body_future: RequestBodyFuture,
        response_body_stream: ReceiverStream<Bytes>,
    },
    WithoutRequestBody {
        response_body_stream: ReceiverStream<Bytes>,
    },
    Done,
}

impl PhpStream {
    fn transition_to_without_request_body(&mut self) {
        match std::mem::replace(self, PhpStream::Done) {
            PhpStream::WithRequestBody {
                response_body_stream,
                ..
            } => {
                *self = PhpStream::WithoutRequestBody {
                    response_body_stream,
                }
            }
            _ => unreachable!(),
        }
    }

    fn transition_to_done(&mut self) {
        let _ = std::mem::replace(self, PhpStream::Done);
    }
}

impl Stream for PhpStream {
    type Item = Result<Frame<Bytes>, PhpError>;

    fn poll_next(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        let this = self.get_mut();

        match this {
            PhpStream::Done => panic!("PhpStream polled after completion"),
            PhpStream::WithRequestBody {
                request_body_future,
                response_body_stream,
            } => {
                // First try to stream a response chunk
                let result = Pin::new(response_body_stream)
                    .poll_next(cx)
                    .map(|chunk| chunk.map(|c| Ok(Frame::data(c))));

                if let Poll::Ready(r) = result {
                    if r.is_none() {
                        // PHP closed the response channel
                        this.transition_to_done();
                    }

                    return Poll::Ready(r);
                }

                // No response chunk ready, try streaming request to PHP
                match Pin::new(request_body_future).poll(cx) {
                    Poll::Pending => Poll::Pending,
                    Poll::Ready(Ok(_)) => {
                        this.transition_to_without_request_body();
                        Poll::Pending
                    }
                    Poll::Ready(Err(e)) => Poll::Ready(Some(Err(e))),
                }
            }
            PhpStream::WithoutRequestBody {
                response_body_stream,
            } => {
                let result = Pin::new(response_body_stream)
                    .poll_next(cx)
                    .map(|chunk| chunk.map(|c| Ok(Frame::data(c))));

                if let Poll::Ready(None) = result {
                    // PHP closed the response channel
                    this.transition_to_done();
                }

                result
            }
        }
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        match self {
            PhpStream::WithRequestBody {
                response_body_stream,
                ..
            } => response_body_stream.size_hint(),
            PhpStream::WithoutRequestBody {
                response_body_stream,
            } => response_body_stream.size_hint(),
            PhpStream::Done => (0, None),
        }
    }
}
