use crate::{
    entities::workspace::ViewTable,
    sqlx_ext::{map_sqlx_error, SqlBuilder},
};
use anyhow::Context;
use chrono::Utc;
use flowy_net::{
    errors::{invalid_params, ServerError},
    response::FlowyResponse,
};
use flowy_workspace::{
    entities::{
        app::parser::AppId,
        view::{
            parser::{ViewDesc, ViewId, ViewName, ViewThumbnail},
            RepeatedView,
            View,
        },
    },
    protobuf::{CreateViewParams, QueryViewParams, UpdateViewParams},
};
use protobuf::ProtobufEnum;
use sqlx::{postgres::PgArguments, PgPool, Postgres, Transaction};
use uuid::Uuid;

pub(crate) async fn create_view(
    pool: &PgPool,
    params: CreateViewParams,
) -> Result<FlowyResponse, ServerError> {
    let name = ViewName::parse(params.name).map_err(invalid_params)?;
    let belong_to_id = AppId::parse(params.belong_to_id).map_err(invalid_params)?;
    let thumbnail = ViewThumbnail::parse(params.thumbnail).map_err(invalid_params)?;
    let desc = ViewDesc::parse(params.desc).map_err(invalid_params)?;

    let mut transaction = pool
        .begin()
        .await
        .context("Failed to acquire a Postgres connection to create view")?;

    let uuid = uuid::Uuid::new_v4();
    let time = Utc::now();

    let (sql, args) = SqlBuilder::create("view_table")
        .add_arg("id", uuid)
        .add_arg("belong_to_id", belong_to_id.as_ref())
        .add_arg("name", name.as_ref())
        .add_arg("description", desc.as_ref())
        .add_arg("modified_time", &time)
        .add_arg("create_time", &time)
        .add_arg("thumbnail", thumbnail.as_ref())
        .add_arg("view_type", params.view_type.value())
        .build()?;

    let _ = sqlx::query_with(&sql, args)
        .execute(&mut transaction)
        .await
        .map_err(map_sqlx_error)?;

    transaction
        .commit()
        .await
        .context("Failed to commit SQL transaction to create view.")?;

    let view = View {
        id: uuid.to_string(),
        belong_to_id: belong_to_id.as_ref().to_owned(),
        name: name.as_ref().to_owned(),
        desc: desc.as_ref().to_owned(),
        view_type: params.view_type.value().into(),
        version: 0,
        belongings: RepeatedView::default(),
    };

    FlowyResponse::success().data(view)
}

pub(crate) async fn read_view(
    pool: &PgPool,
    params: QueryViewParams,
) -> Result<FlowyResponse, ServerError> {
    let view_id = check_view_id(params.view_id)?;
    let mut transaction = pool
        .begin()
        .await
        .context("Failed to acquire a Postgres connection to read view")?;

    let (sql, args) = SqlBuilder::select("view_table")
        .add_field("*")
        .and_where_eq("id", view_id)
        .build()?;

    let table = sqlx::query_as_with::<Postgres, ViewTable, PgArguments>(&sql, args)
        .fetch_one(&mut transaction)
        .await
        .map_err(map_sqlx_error)?;

    let mut views = RepeatedView::default();
    if params.read_belongings {
        views.items = read_views_belong_to_id(&mut transaction, &table.id.to_string()).await?;
    }

    transaction
        .commit()
        .await
        .context("Failed to commit SQL transaction to read view.")?;

    let mut view: View = table.into();
    view.belongings = views;

    FlowyResponse::success().data(view)
}

pub(crate) async fn update_view(
    pool: &PgPool,
    params: UpdateViewParams,
) -> Result<FlowyResponse, ServerError> {
    let view_id = check_view_id(params.view_id.clone())?;

    let name = match params.has_name() {
        false => None,
        true => Some(
            ViewName::parse(params.get_name().to_owned())
                .map_err(invalid_params)?
                .0,
        ),
    };

    let desc = match params.has_desc() {
        false => None,
        true => Some(
            ViewDesc::parse(params.get_desc().to_owned())
                .map_err(invalid_params)?
                .0,
        ),
    };

    let thumbnail = match params.has_thumbnail() {
        false => None,
        true => Some(
            ViewThumbnail::parse(params.get_thumbnail().to_owned())
                .map_err(invalid_params)?
                .0,
        ),
    };

    let mut transaction = pool
        .begin()
        .await
        .context("Failed to acquire a Postgres connection to update app")?;

    let (sql, args) = SqlBuilder::update("view_table")
        .add_some_arg("name", name)
        .add_some_arg("description", desc)
        .add_some_arg("thumbnail", thumbnail)
        .add_some_arg("modified_time", Some(Utc::now()))
        .add_arg_if(params.has_is_trash(), "is_trash", params.get_is_trash())
        .and_where_eq("id", view_id)
        .build()?;

    sqlx::query_with(&sql, args)
        .execute(&mut transaction)
        .await
        .map_err(map_sqlx_error)?;

    transaction
        .commit()
        .await
        .context("Failed to commit SQL transaction to update view.")?;

    Ok(FlowyResponse::success())
}

pub(crate) async fn delete_view(
    pool: &PgPool,
    view_id: &str,
) -> Result<FlowyResponse, ServerError> {
    let view_id = check_view_id(view_id.to_owned())?;
    let mut transaction = pool
        .begin()
        .await
        .context("Failed to acquire a Postgres connection to delete view")?;

    let (sql, args) = SqlBuilder::delete("view_table")
        .and_where_eq("id", view_id)
        .build()?;

    let _ = sqlx::query_with(&sql, args)
        .execute(&mut transaction)
        .await
        .map_err(map_sqlx_error)?;

    transaction
        .commit()
        .await
        .context("Failed to commit SQL transaction to delete view.")?;

    Ok(FlowyResponse::success())
}

// transaction must be commit from caller
pub(crate) async fn read_views_belong_to_id<'c>(
    transaction: &mut Transaction<'c, Postgres>,
    id: &str,
) -> Result<Vec<View>, ServerError> {
    // TODO: add index for app_table
    let (sql, args) = SqlBuilder::select("view_table")
        .add_field("*")
        .and_where_eq("belong_to_id", id)
        .build()?;

    let tables = sqlx::query_as_with::<Postgres, ViewTable, PgArguments>(&sql, args)
        .fetch_all(transaction)
        .await
        .map_err(map_sqlx_error)?;

    let views = tables
        .into_iter()
        .map(|table| table.into())
        .collect::<Vec<View>>();

    Ok(views)
}

fn check_view_id(id: String) -> Result<Uuid, ServerError> {
    let view_id = ViewId::parse(id).map_err(invalid_params)?;
    let view_id = Uuid::parse_str(view_id.as_ref())?;
    Ok(view_id)
}
