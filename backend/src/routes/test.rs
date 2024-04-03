use rocket::{
    serde::json::Json,
    http::Status,
    // response::Responder,
    response::status,
    form::Form,
    fs::TempFile
    // State, data::{Limits, ToByteUnit}
};
use rocket::http::{
    ContentType,
    MediaType
};
use tokio::io::AsyncReadExt;
use log;
use std::{
    path,
    fs
};
// use std::{path, time::Duration};
// use std::fs;
use bb8;
use bb8_redis;
use redis;
use s3::Bucket;
use rsmq_async::{PooledRsmq, RsmqConnection};
// use crate::enums::rsmq::RsmqDsQueue;
use crate::utils;

// use crate::models::uploads;


pub fn routes() -> Vec<rocket::Route> {
    routes![
        test_redis,
        test_s3,
        test_rsmq_send,
        test_rsmq_receive
    ]
}

#[derive(serde::Deserialize, Debug)]
struct TestRedis {
    key: String,
    value: String
}

#[derive(serde::Serialize, Debug)]
struct TestRedisResponse {
    success: bool,
    detail: Option<String>
}

#[derive(FromForm)]
struct TestS3<'f> {
    file: TempFile<'f>,
    tags: Option<String>
}

#[derive(serde::Serialize, Debug)]
struct TestS3Response {
    success: bool,
    detail: Option<String>
}

#[derive(serde::Deserialize, Debug)]
struct TestRsmq {
    queue_name: String,
    message: String
}

#[derive(serde::Serialize, Debug)]
struct TestRsmqResponse {
    success: bool,
    queue_id: Option<String>,
    detail: Option<String>
}

#[derive(serde::Deserialize, Debug)]
struct TestRsmqReceive {
    queue_name: String
}

#[derive(serde::Serialize, Debug)]
struct TestRsmqReceiveResponse {
    success: bool,
    message: Option<String>,
    detail: Option<String>
}

#[post("/redis", format = "application/json", data = "<data>")]
async fn test_redis(data: Json<TestRedis>, pool: &rocket::State<bb8::Pool<bb8_redis::RedisConnectionManager>>) -> status::Custom<Json<TestRedisResponse>> {
    log::debug!("Got a request: {:?}", data);

    // Send data to Redis
    log::debug!("Sending data to Redis...");
    let mut conn = match pool.get().await {
        Ok(conn) => conn,
        Err(e) => {
            log::error!("Failed to get connection from Redis pool: {}", e);
            // Return status code 500
            return status::Custom(
                Status::InternalServerError,
                Json(TestRedisResponse {
                    success: false,
                    detail: Some(format!("Failed to get connection from Redis pool: {}", e))
                })
            )
        }
    };

    let _: () = match redis::cmd("SET")
        .arg(format!("test:redis:{}", &data.key))
        .arg(&data.value)
        .arg("EX")
        .arg("10")
        .query_async(&mut *conn)
        .await {
            Ok(()) => (),
            Err(e) => {
                log::error!("Failed to send data to Redis: {}", e);
                // Return status code 500
                return status::Custom(
                    Status::InternalServerError,
                    Json(TestRedisResponse {
                        success: false,
                        detail: Some(format!("Failed to send data to Redis: {}", e))
                    })
                )
            }
        };

    log::debug!("Data sent to Redis.");

    status::Custom(
        Status::Ok,
        Json(TestRedisResponse {
            success: true,
            detail: None
        })
    )
}

#[post("/s3", format = "multipart/form-data", data = "<form>")]
async fn test_s3(form: Form<TestS3<'_>>, bucket: &rocket::State<Bucket>) -> status::Custom<Json<TestS3Response>> {
    log::debug!("Got a request: {:?}", form.file);

    // Test tags
    match &form.tags {
        Some(tags) => {
            log::debug!("Got tags: {:?}", tags);
        },
        None => {
            log::debug!("No tags provided.");
        }
    }

    // Create a proper file path
    let file_type: &ContentType = match form.file.content_type() {
        Some(t) => t,
        None => {
            log::error!("Failed to get file type.");
            return status::Custom(
                Status::BadRequest,
                Json(TestS3Response {
                    success: false,
                    detail: Some("Failed to get file type.".into())
                })
            );
        }
    };

    // Create a proper file path
    let mut file_buffer: Vec<u8> = Vec::new();
    form.file.open().await.unwrap().read_buf(&mut file_buffer).await.unwrap();

    let file_name: String;
    let file_path: String;
    {
        let media_type: &MediaType = file_type.media_type();
        let file_hash = crate::utils::hash::hash_file(
            &file_buffer
        );

        let file_ext;
        if media_type.sub() == "jpeg" {
            file_ext = "jpg";
        } else {
            file_ext = media_type.sub().as_str();
        }

        file_name = format!("{}.{}", file_hash, file_ext);
        file_path = file_name.clone();  // For flexibility
        log::debug!("File will be saved in S3 as {}", &file_path);
    }
    log::debug!("Sending data to S3...");
    let mut file_buffer = form.file.open().await.unwrap();
    match bucket.put_object_stream(&mut file_buffer, &file_path).await {
        Ok(_) => (),
        Err(e) => {
            log::error!("Failed to send data to S3: {}", e);
            // Return status code 500
            return status::Custom(
                Status::InternalServerError,
                Json(TestS3Response {
                    success: false,
                    detail: Some(format!("Failed to send data to S3: {}", e))
                })
            )
        }
    };

    log::debug!("Data sent to S3.");

    status::Custom(
        Status::Ok,
        Json(TestS3Response {
            success: true,
            detail: None
        })
    )
}

#[post("/rsmq/send", data = "<data>")]
async fn test_rsmq_send(data: Json<TestRsmq>, pool: &rocket::State<PooledRsmq>) -> status::Custom<Json<TestRsmqResponse>> {
    log::debug!("Got a request: {:?}", data);

    // Verify queue name
    if !utils::rsmq::verify::queue_exists(data.queue_name.as_str()) {
        log::error!("Queue name is invalid.");
        return status::Custom(
            Status::BadRequest,
            Json(TestRsmqResponse {
                success: false,
                queue_id: None,
                detail: Some("Queue name is invalid.".into())
            })
        )
    }

    // Send data to RSMQ
    log::debug!("Sending data to RSMQ...");
    let queue_id = match pool.inner().clone().send_message(
        data.queue_name.as_str(),
        data.message.as_str(),
        None
    ).await {
        Ok(msg) => msg,
        Err(e) => {
            log::error!("Failed to send data to RSMQ: {}", e);
            return status::Custom(
                Status::InternalServerError,
                Json(TestRsmqResponse {
                    success: false,
                    queue_id: None,
                    detail: Some(format!("Failed to send data to RSMQ: {}", e))
                })
            )
        }
    };


    status::Custom(
        Status::Ok,
        Json(TestRsmqResponse {
            success: true,
            queue_id: Some(queue_id),
            detail: None
        })
    )
}

#[post("/rsmq/receive", data = "<data>")]
async fn test_rsmq_receive(data: Json<TestRsmqReceive>, pool: &rocket::State<PooledRsmq>) -> status::Custom<Json<TestRsmqReceiveResponse>> {
    log::debug!("Got a request: {:?}", data);

    // Verify queue name
    if !utils::rsmq::verify::queue_exists(data.queue_name.as_str()) {
        log::error!("Queue name is invalid.");
        return status::Custom(
            Status::BadRequest,
            Json(TestRsmqReceiveResponse {
                success: false,
                message: None,
                detail: Some("Queue name is invalid.".into())
            })
        )
    }

    // Receive data from RSMQ
    log::debug!("Receiving data from RSMQ...");
    let message = match pool.inner().clone().receive_message(
        data.queue_name.as_str(),
        None
    ).await {
        Ok(msg) => {
            let msg = match msg {
                Some(m) => m,
                None => {
                    log::error!("Failed to receive data from RSMQ: No message received.");
                    return status::Custom(
                        Status::InternalServerError,
                        Json(TestRsmqReceiveResponse {
                            success: false,
                            message: None,
                            detail: Some("Failed to receive data from RSMQ: No message received.".into())
                        })
                    )
                }
            };
            log::debug!("Removing message from RSMQ: {:?}", msg);
            pool.inner().clone().delete_message(data.queue_name.as_str(), &msg.id).await.unwrap();
            log::debug!("Message removed from Redis: {:?}", msg);
            msg
        }
        Err(e) => {
            log::error!("Failed to receive data from RSMQ: {}", e);
            return status::Custom(
                Status::InternalServerError,
                Json(TestRsmqReceiveResponse {
                    success: false,
                    message: None,
                    detail: Some(format!("Failed to receive data from RSMQ: {}", e))
                })
            )
        }
    };

    status::Custom(
        Status::Ok,
        Json(TestRsmqReceiveResponse {
            success: true,
            message: Some(message.message),
            detail: None
        })
    )
}