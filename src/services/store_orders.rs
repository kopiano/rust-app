use std::{sync::Arc, time::Duration};

use lapin::{
    BasicProperties, Connection, ConnectionProperties, ExchangeKind,
    options::{
        BasicAckOptions, BasicConsumeOptions, BasicPublishOptions, BasicQosOptions,
        ConfirmSelectOptions, ExchangeDeclareOptions, QueueBindOptions, QueueDeclareOptions,
    },
    types::FieldTable,
};
use serde::Deserialize;
use sqlx::PgPool;
use tokio::sync::Notify;
use uuid::Uuid;

const EXCHANGE: &str = "store.events";
const QUEUE: &str = "store.order.created";
const ROUTING_KEY: &str = "store.order.created";

pub async fn run_outbox_publisher(db: PgPool, notify: Arc<Notify>) {
    loop {
        let url = match std::env::var("AMQP_URL") {
            Ok(url) => url,
            Err(_) => {
                tracing::warn!("AMQP_URL is not configured; order outbox publisher is idle");
                notify.notified().await;
                continue;
            }
        };
        match Connection::connect(&url, ConnectionProperties::default()).await {
            Ok(connection) => {
                if let Err(error) = publish_pending(&db, &connection, notify.clone()).await {
                    tracing::error!(%error, "Order outbox publisher stopped");
                }
            }
            Err(error) => {
                tracing::error!(%error, "Unable to connect order outbox publisher to RabbitMQ")
            }
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn publish_pending(
    db: &PgPool,
    connection: &Connection,
    notify: Arc<Notify>,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let channel = connection.create_channel().await?;
    channel
        .confirm_select(ConfirmSelectOptions::default())
        .await?;
    channel
        .exchange_declare(
            EXCHANGE.into(),
            ExchangeKind::Topic,
            ExchangeDeclareOptions {
                durable: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;
    channel
        .queue_declare(
            QUEUE.into(),
            QueueDeclareOptions {
                durable: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;
    channel
        .queue_bind(
            QUEUE.into(),
            EXCHANGE.into(),
            ROUTING_KEY.into(),
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await?;

    loop {
        let rows = sqlx::query_as::<_, (Uuid, String, serde_json::Value)>(
            "SELECT event_id, event_type, payload
             FROM store_order_outbox WHERE published_at IS NULL
             ORDER BY created_at ASC LIMIT 50",
        )
        .fetch_all(db)
        .await?;

        let has_rows = !rows.is_empty();
        for (event_id, event_type, payload) in rows {
            let body = serde_json::to_vec(&payload)?;
            let confirmation = channel
                .basic_publish(
                    EXCHANGE.into(),
                    event_type.as_str().into(),
                    BasicPublishOptions {
                        mandatory: true,
                        ..Default::default()
                    },
                    &body,
                    BasicProperties::default()
                        .with_delivery_mode(2)
                        .with_message_id(event_id.to_string().into()),
                )
                .await?
                .await?;
            if !confirmation.is_ack() || confirmation.take_message().is_some() {
                return Err("RabbitMQ did not confirm a routable store order event".into());
            }
            sqlx::query("UPDATE store_order_outbox SET published_at = NOW() WHERE event_id = $1")
                .bind(event_id)
                .execute(db)
                .await?;
        }

        if !has_rows {
            tokio::select! {
                _ = notify.notified() => {},
                _ = tokio::time::sleep(Duration::from_secs(2)) => {},
            }
        }
    }
}

#[derive(Deserialize)]
struct OrderCreated {
    order_id: Uuid,
}

pub async fn run_consumer(db: PgPool) {
    loop {
        let Ok(url) = std::env::var("AMQP_URL") else {
            tracing::warn!("AMQP_URL is not configured; store order consumer is idle");
            tokio::time::sleep(Duration::from_secs(10)).await;
            continue;
        };
        match consume(&url, &db).await {
            Ok(()) => tracing::warn!("Order consumer stopped; reconnecting"),
            Err(error) => tracing::error!(%error, "Order consumer failed; reconnecting"),
        }
        tokio::time::sleep(Duration::from_secs(3)).await;
    }
}

async fn consume(url: &str, db: &PgPool) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    let connection = Connection::connect(url, ConnectionProperties::default()).await?;
    let channel = connection.create_channel().await?;
    channel
        .exchange_declare(
            EXCHANGE.into(),
            ExchangeKind::Topic,
            ExchangeDeclareOptions {
                durable: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;
    channel
        .queue_declare(
            QUEUE.into(),
            QueueDeclareOptions {
                durable: true,
                ..Default::default()
            },
            FieldTable::default(),
        )
        .await?;
    channel
        .queue_bind(
            QUEUE.into(),
            EXCHANGE.into(),
            ROUTING_KEY.into(),
            QueueBindOptions::default(),
            FieldTable::default(),
        )
        .await?;
    channel.basic_qos(1, BasicQosOptions::default()).await?;
    let mut consumer = channel
        .basic_consume(
            QUEUE.into(),
            "store-order-worker".into(),
            BasicConsumeOptions::default(),
            FieldTable::default(),
        )
        .await?;
    use futures_util::StreamExt;
    while let Some(delivery) = consumer.next().await {
        let delivery = delivery?;
        let event: OrderCreated = match serde_json::from_slice(&delivery.data) {
            Ok(event) => event,
            Err(error) => {
                tracing::error!(%error, "Invalid store order event");
                delivery.ack(BasicAckOptions::default()).await?;
                continue;
            }
        };
        if let Err(error) = advance_order(db, &event.order_id).await {
            tracing::error!(%error, order_id = %event.order_id, "Failed to advance store order");
            delivery
                .nack(lapin::options::BasicNackOptions {
                    requeue: true,
                    ..Default::default()
                })
                .await?;
            continue;
        }
        delivery.ack(BasicAckOptions::default()).await?;
    }
    Ok(())
}

async fn advance_order(db: &PgPool, order_id: &Uuid) -> Result<(), sqlx::Error> {
    let status = sqlx::query_scalar::<_, String>("SELECT status FROM store_order WHERE id = $1")
        .bind(order_id)
        .fetch_optional(db)
        .await?;
    match status.as_deref() {
        Some("pending") => {
            sqlx::query("UPDATE store_order SET status = 'processing', updated_at = NOW() WHERE id = $1 AND status = 'pending'")
                .bind(order_id).execute(db).await?;
            tokio::time::sleep(Duration::from_millis(350)).await;
        }
        Some("processing") => {}
        Some("confirmed" | "failed") | None => return Ok(()),
        Some(_) => {}
    }
    sqlx::query("UPDATE store_order SET status = 'confirmed', updated_at = NOW() WHERE id = $1 AND status = 'processing'")
        .bind(order_id)
        .execute(db)
        .await?;
    Ok(())
}
