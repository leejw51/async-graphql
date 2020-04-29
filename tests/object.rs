use async_graphql::*;

#[async_std::test]
pub async fn test_object_field_flatten() {
    #[SimpleObject]
    struct MyObj {
        a: i32,
        b: i32,
        value_aa: i32,
    }

    struct Query;

    #[Object]
    impl Query {
        #[field(flatten)]
        async fn obj(&self) -> MyObj {
            MyObj {
                a: 1,
                b: 2,
                value_aa: 99,
            }
        }

        async fn c(&self) -> i32 {
            3
        }
    }

    let schema = Schema::new(Query, EmptyMutation, EmptySubscription);
    let query = "{ a b valueAa c }";
    assert_eq!(
        schema.execute(&query).await.unwrap().data,
        serde_json::json!({
            "a": 1, "b": 2, "valueAa": 99, "c": 3
        })
    );
}

#[async_std::test]
pub async fn test_simple_object_field_flatten() {
    #[SimpleObject]
    struct MyObj {
        a: i32,
        b: i32,
    }

    #[SimpleObject]
    struct Query {
        #[field(flatten)]
        obj: MyObj,
        c: i32,
    }

    let schema = Schema::new(
        Query {
            obj: MyObj { a: 1, b: 2 },
            c: 3,
        },
        EmptyMutation,
        EmptySubscription,
    );
    let query = "{ a b c }";
    assert_eq!(
        schema.execute(&query).await.unwrap().data,
        serde_json::json!({
            "a": 1, "b": 2, "c": 3
        })
    );
}

#[async_std::test]
pub async fn test_object_field_flatten_interface() {
    #[SimpleObject]
    struct MyObj {
        a: i32,
        b: i32,
    }

    struct Query;

    #[SimpleObject]
    struct MyObj2 {
        #[field(flatten)]
        obj: MyObj,
        c: i32,
    }

    #[Interface(field(name = "a", type = "i32"))]
    struct MyInterface(MyObj2);

    #[Object]
    impl Query {
        async fn obj(&self) -> MyInterface {
            MyObj2 {
                obj: MyObj { a: 1, b: 2 },
                c: 3,
            }
            .into()
        }
    }

    let schema = Schema::new(Query, EmptyMutation, EmptySubscription);
    let query = r#"{
        obj {
            ... on MyObj2 {
                a b c
            }
        }
        obj1:obj {
            ... on MyInterface {
                a
            }
            ... on MyObj2 {
                b c
            }
        }
    }"#;
    assert_eq!(
        schema.execute(&query).await.unwrap().data,
        serde_json::json!({
            "obj" : {"a": 1, "b": 2, "c": 3},
            "obj1" : {"a": 1, "b": 2, "c": 3}
        })
    );
}
