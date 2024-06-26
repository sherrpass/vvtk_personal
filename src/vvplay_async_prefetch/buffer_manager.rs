use crate::dash::buffer::{Buffer, FrameStatus};
use crate::dash::ViewportPrediction;
use crate::formats::pointxyzrgba::PointXyzRgba;
use crate::formats::PointCloud;
use crate::render::wgpu::{camera::CameraPosition, reader::FrameRequest};
use crate::vvplay_async_prefetch::camera_trace::CameraTrace;
use crate::vvplay_async_prefetch::fetch_request::FetchRequest;
use crate::BufMsg;

/**
 * This file contains Buffer Manager struct and related implementation
 */

/// Buffer Manager handles 2 interactions:
/// 1. Fetcher & Decoder: buffer manager sends request to source data (either from the network or from the local filesystem).
/// It expects to get a PointCloud back, which it will put into its buffer until the renderer is ready to consume it.
/// 2. Renderer: buffer manager receives request for point cloud from the renderer and returns the (assembled) point cloud to the renderer.
///
/// The interaction flow:
/// - Buffer manager receives a request from the renderer.
/// - Buffer manager checks if the requested point cloud is in its buffer.
///     - If it is, buffer manager returns the point cloud to the renderer.
///     - If it is not, buffer manager sends a request to the source data (either from the network or from the local filesystem).
/// - Buffer manager receives the point cloud from the decoder and puts it into its buffer.
/// - Buffer manager returns the point cloud to the renderer.
pub struct BufferManager {
    //to_buf_rx receive any buffer message
    to_buf_rx: tokio::sync::mpsc::UnboundedReceiver<BufMsg>,
    //buf_in_sx is used to send FetchRequest for local or remote source
    buf_in_sx: tokio::sync::mpsc::UnboundedSender<FetchRequest>,
    buf_out_sx: std::sync::mpsc::Sender<(FrameRequest, PointCloud<PointXyzRgba>)>,
    /// frame_to_answer is the frame we are pending to answer to the renderer.
    /// Note(25Mar23): it is an option because we are only dealing with 1 object_id for now.
    frame_to_answer: Option<FrameRequest>,
    /// buffer stores all requests, it might be in fetching or decoding or ready state.
    buffer: Buffer,
    total_frames: usize,
    segment_size: u64,
    shutdown_recv: tokio::sync::watch::Receiver<bool>,
}

impl BufferManager {
    pub fn new(
        to_buf_rx: tokio::sync::mpsc::UnboundedReceiver<BufMsg>,
        buf_in_sx: tokio::sync::mpsc::UnboundedSender<FetchRequest>,
        buf_out_sx: std::sync::mpsc::Sender<(FrameRequest, PointCloud<PointXyzRgba>)>,
        buffer_size: u64,
        total_frames: usize,
        segment_size: (u64, u64),
        shutdown_recv: tokio::sync::watch::Receiver<bool>,
    ) -> Self {
        BufferManager {
            to_buf_rx,
            buf_in_sx,
            buf_out_sx,
            frame_to_answer: None,
            total_frames,
            segment_size: segment_size.0,
            shutdown_recv,
            // buffer size is given in seconds. however our frames are only segment_size.0 / segment_size.1 seconds long.
            buffer: Buffer::new(buffer_size as usize),
        }
    }

    /// Get next frame request assuming playback is continuous
    pub fn get_next_frame_req(&self, req: &FrameRequest) -> FrameRequest {
        FrameRequest {
            object_id: req.object_id,
            frame_offset: (req.frame_offset + self.segment_size) % self.total_frames as u64,
            camera_pos: req.camera_pos,
        }
    }

    //Send fetch request for the next frame and add it to the buffer
    pub fn prefetch_frame(&mut self, camera_pos: Option<CameraPosition>) {
        assert!(camera_pos.is_some());
        let last_req = FrameRequest {
            camera_pos,
            ..self.buffer.back().unwrap().req
        };
        // The frame prefetched is the next frame of the frame at the back of the buffer
        let req = self.get_next_frame_req(&last_req);
        _ = self
            .buf_in_sx
            .send(FetchRequest::new(req, self.buffer.len()));
        //println!("In prefetch_frame, the request is {:?}", req);

        self.buffer.add(req);
    }

    // Overloading prefetch_frame so we can specify which frame to be prefetched
    pub fn prefetch_frame_with_request(
        &mut self,
        camera_pos: Option<CameraPosition>,
        last_req: FrameRequest,
    ) {
        assert!(camera_pos.is_some());
        let req = self.get_next_frame_req(&last_req);
        _ = self
            .buf_in_sx
            .send(FetchRequest::new(req, self.buffer.len()));
        //println!("In prefetch_frame_with_request, the request is {:?}", req);

        self.buffer.add(req);
    }

    pub async fn run(
        &mut self,
        mut viewport_predictor: Box<dyn ViewportPrediction>,
        original_position: CameraPosition,
        camera_trace: Option<CameraTrace>,
        mut record_camera_trace: Option<CameraTrace>,
    ) {
        // Since we prefetch after a `FetchDone` event, once the buffer is full, we can't prefetch anymore.
        // So, we set this flag to true once the buffer is full, so that when the frames are consumed and the first channels are discarded, we can prefetch again.
        let mut is_desired_buffer_level_reached = false;
        let mut last_req: Option<FrameRequest> = None;
        loop {
            /*
            println!{"---------------------------"};
            println!("buffer: {:?}", &self.buffer);
            */
            //wait for message in self.shutdown_recv and self.to_buf_Rx
            //if a message is received, match the message with the bufmsg enum
            if !self.buffer.is_full() && !self.buffer.is_empty() {
                self.prefetch_frame(Some(CameraPosition::default()));
            } else if self.buffer.is_empty() && last_req.is_some() {
                //temporary fix: right not just assign default camera position
                self.prefetch_frame_with_request(
                    Some(CameraPosition::default()),
                    last_req.unwrap(),
                );
            }
            tokio::select! {
                _ = self.shutdown_recv.changed() => {
                    /*
                    println!{"---------------------------"};
                    println!{"in vvplay_async:"}
                    println!{"[buffer mgr] received shutdown signal"};
                    */
                    break;
                }
                Some(msg) = self.to_buf_rx.recv() => {
                    match msg {
                        BufMsg::FrameRequest(mut renderer_req) => {
                            /*
                            println!{"---------------------------"};
                            println!{"[buffer mgr] renderer sent a frame request {:?}", &renderer_req};
                            */
                            // record camera trace
                            if record_camera_trace.is_some() && renderer_req.camera_pos.is_some() {
                                if let Some(ct) = record_camera_trace.as_mut() { ct.add(renderer_req.camera_pos.unwrap()) }
                            }

                            // If the camera trace is provided, we will use the camera trace to override the camera position for the next frame
                            // else we will feed this into the viewport predictor
                            if camera_trace.is_some() {
                                renderer_req.camera_pos = camera_trace.as_ref().map(|ct| ct.next());
                            } else {
                                viewport_predictor.add(renderer_req.camera_pos.unwrap_or_else(|| original_position));
                                renderer_req.camera_pos = viewport_predictor.predict();
                            }

                            // First, attempt to fulfill the request from the buffer.
                            // Check in cache whether it exists
                            if !self.buffer.is_empty() && self.buffer.front().unwrap().req.frame_offset == renderer_req.frame_offset {
                                let mut front = self.buffer.pop_front().unwrap();
                                match front.state {
                                    FrameStatus::Fetching | FrameStatus::Decoding => {
                                        // we update frame_to_answer to indicate that we are waiting to send back this data to renderer.
                                        self.frame_to_answer = Some(renderer_req);
                                        self.buffer.push_front(front);
                                    }
                                    FrameStatus::Ready(remaining_frames, mut rx) => {
                                        // send to the renderer
                                        match rx.recv().await {
                                            Some(pc) => {
                                                // if camera trace is not provided, we should not send camera_pos back to the renderer
                                                // as it is just a prediction, not an instruction to move to that position
                                                let original_camera_pos = if camera_trace.is_none() {
                                                    renderer_req.camera_pos.take()
                                                } else {
                                                    renderer_req.camera_pos
                                                };
                                                // send to point cloud to renderer
                                                _ = self.buf_out_sx.send((renderer_req, pc));
                                                self.frame_to_answer = None;
                                                front.req.frame_offset += 1;
                                                front.state = FrameStatus::Ready(remaining_frames - 1, rx);
                                                //println!("In FrameStatus::Ready, the front is {:?}", front);
                                                if remaining_frames > 1 {
                                                    // we only reinsert it if there are more frames to render
                                                    self.buffer.push_front(front);
                                                } else if !is_desired_buffer_level_reached {
                                                    //println!("in FrameStatus::Ready::!is_desired_buffer_level_reached");
                                                    //if the desired buffer level is not reached, should add in a new frame
                                                    self.prefetch_frame(original_camera_pos);
                                                }
                                            }
                                            None => {
                                                unreachable!("we should never have an empty channel");
                                                // channel is empty, so we discard this channel
                                                // we update frame_to_answer to indicate that we are waiting to send back this data to renderer.
                                                // self.frame_to_answer = Some(renderer_req);
                                            }
                                        }
                                    }
                                }
                            } else {
                                // It has not been requested, so we send a request to the fetcher to fetch the data
                                _ = self.buf_in_sx.send(FetchRequest::new(renderer_req, self.buffer.len()));

                                // we update frame_to_answer to indicate that we are waiting to send back this data to renderer.
                                self.frame_to_answer = Some(renderer_req);

                                // we also update next_fetch_req so that when the fetcher returns the data, we can immediately send the next request to the fetcher
                                self.buffer.add(renderer_req);
                            }
                        }
                        BufMsg::FetchDone(req) => {
                            // upon receiving fetch result, immediately schedule the next fetch request
                            /*
                            println!{"---------------------------"};
                            println!("the current buffer message is fetch done for {:?}", req);
                            */
                            self.buffer.update_state(req, FrameStatus::Decoding);

                            if !self.buffer.is_full() {
                                // If the buffer is not full yet, we can send a request to the fetcher to fetch the next frame
                                self.prefetch_frame(req.camera_pos);
                            } else {
                                is_desired_buffer_level_reached = true;
                            }
                        }
                        BufMsg::PointCloud((mut metadata, mut rx)) => {
                            /*
                            println!{"---------------------------"};
                            println!("[buffer mgr] received a point cloud result {:?}", &metadata);
                             */
                            let orig_metadata: FrameRequest = metadata.into();
                            //if this frame is the one that the renderer is awaiting, do not put it back and send it to the renderer
                            let mut remaining = self.segment_size as usize;
                            if self.frame_to_answer.is_some()
                                && metadata.frame_offset
                                    == self.frame_to_answer.as_ref().unwrap().frame_offset
                            {
                                let pc = rx.recv().await.unwrap();
                                // send results to the renderer
                                _ = self.buf_out_sx.send((self.frame_to_answer.unwrap(), pc));
                                self.frame_to_answer = None;
                                metadata.frame_offset += 1;
                                remaining -= 1;
                            }
                            // cache the point cloud if there is still point clouds to render
                            self.buffer.update(orig_metadata, metadata.into(), FrameStatus::Ready(remaining, rx));
                            last_req = Some(orig_metadata);
                        }
                    }
                }
                else => break,
            }
        }
    }
}
