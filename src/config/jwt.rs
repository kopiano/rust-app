pub struct JwtConfig {
    pub secret: String,
    pub max_age: i64,
}

impl JwtConfig {
    pub fn from_env() -> Self {
        Self {
            secret: std::env::var("JWT_SECRET_KEY").expect("JWT_SECRET_KEY not found"),
            max_age: std::env::var("JWT_MAXAGE").expect("JWT_MAXAGE must be a number").parse::<i64>().unwrap(),
        }
    }
}
